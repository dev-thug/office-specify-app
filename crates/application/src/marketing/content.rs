//! Pure rendering of immutable Office templates for a Zoho Campaigns Free
//! campaign. Office remains the source of truth for both the template and
//! recipient-specific values; this module only produces provider-ready
//! campaign content and the field values needed to resolve native merge tags.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

const MAX_CAMPAIGN_CONTENT_CHARS: usize = 300_000;
const ORG_OPTOUT_TAG: &str = "$[LI:ORG_OPTOUT]$";

/// Values resolved for one recipient from the frozen launch snapshot.
pub type RecipientVariables = BTreeMap<String, String>;

/// A registered Campaigns contact field and its native merge tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CampaignFieldMapping {
    pub contact_field: String,
    pub merge_tag: String,
}

/// Immutable input to [`transform_campaign_content`]. `allowed_contact_fields`
/// is the set of labels supplied by the caller after provider-field validation.
#[derive(Debug, Clone, Copy)]
pub struct CampaignContentInput<'a> {
    pub subject: &'a str,
    pub text: &'a str,
    pub html: Option<&'a str>,
    pub recipients: &'a [RecipientVariables],
    pub mappings: &'a BTreeMap<String, CampaignFieldMapping>,
    pub allowed_contact_fields: &'a BTreeSet<String>,
}

/// Campaign-wide content and recipient-aligned contact field values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CampaignContent {
    pub subject: String,
    pub text: String,
    pub html: Option<String>,
    /// One map per input recipient, in the same order as the frozen snapshot.
    pub recipient_fields: Vec<BTreeMap<String, String>>,
    /// SHA-256 of the complete transformed content and aligned field values.
    pub content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CampaignContentError {
    #[error("campaign content requires at least one recipient")]
    EmptyRecipients,
    #[error("{part} exceeds the {max} character limit")]
    ContentTooLong { part: &'static str, max: usize },
    #[error("subject must not contain CR or LF")]
    HeaderBreak,
    #[error("template contains an invalid placeholder: {0}")]
    InvalidPlaceholder(String),
    #[error("recipient {recipient_index} has no value for '{variable}'")]
    MissingRecipientValue {
        recipient_index: usize,
        variable: String,
    },
    #[error("recipient values for '{0}' differ and require an explicit field mapping")]
    MissingMapping(String),
    #[error("invalid mapping for '{variable}': {reason}")]
    InvalidMapping { variable: String, reason: String },
    #[error("recipient value for '{variable}' is unsafe in {context}: {reason}")]
    UnsafePersonalization {
        variable: String,
        context: &'static str,
        reason: &'static str,
    },
    #[error("template contains multiple unsubscribe locations in {part}")]
    MultipleUnsubscribe { part: &'static str },
    #[error("template contains a hardcoded external unsubscribe link")]
    ExternalUnsubscribe,
    #[error("template contains an unexpected native merge tag: {0}")]
    UnexpectedNativeMergeTag(String),
    #[error("HTML contains disallowed active markup or an unsafe URL")]
    UnsafeHtml,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Replacement {
    Static(String),
    Native(String),
}

/// Transform an Office-owned template and frozen recipient snapshot into
/// shared Campaigns content. Identical values are safely rendered as literals;
/// differing values require an explicitly registered native merge mapping.
pub fn transform_campaign_content(
    input: &CampaignContentInput<'_>,
) -> Result<CampaignContent, CampaignContentError> {
    if input.recipients.is_empty() {
        return Err(CampaignContentError::EmptyRecipients);
    }
    check_content_length(input.subject, "subject")?;
    check_content_length(input.text, "text")?;
    if let Some(html) = input.html {
        check_content_length(html, "html")?;
    }
    if domain::templates::contains_header_break(input.subject) {
        return Err(CampaignContentError::HeaderBreak);
    }

    let total_source_chars = input.subject.chars().count()
        + input.text.chars().count()
        + input.html.map_or(0, |html| html.chars().count());
    if total_source_chars > MAX_CAMPAIGN_CONTENT_CHARS {
        return Err(CampaignContentError::ContentTooLong {
            part: "campaign content",
            max: MAX_CAMPAIGN_CONTENT_CHARS,
        });
    }

    if input.html.is_some_and(unsafe_html_template_source) {
        return Err(CampaignContentError::UnsafeHtml);
    }
    if [input.subject, input.text, input.html.unwrap_or("")]
        .iter()
        .any(|part| has_external_unsubscribe_link(part))
    {
        return Err(CampaignContentError::ExternalUnsubscribe);
    }

    let subject_vars = parse_placeholders(input.subject)?;
    let text_vars = parse_placeholders(input.text)?;
    let html_vars = input
        .html
        .map(parse_placeholders)
        .transpose()?
        .unwrap_or_default();
    let all_vars: BTreeSet<String> = subject_vars
        .iter()
        .chain(text_vars.iter())
        .chain(html_vars.iter())
        .cloned()
        .collect();

    if subject_vars
        .iter()
        .any(|name| name == "compliance.unsubscribe_url")
    {
        return Err(CampaignContentError::InvalidPlaceholder(
            "compliance.unsubscribe_url cannot appear in the subject".into(),
        ));
    }
    if input.subject.contains(ORG_OPTOUT_TAG) {
        return Err(CampaignContentError::InvalidPlaceholder(
            "the native opt-out tag cannot appear in the subject".into(),
        ));
    }

    validate_source_native_tags(input.subject, "subject")?;
    validate_source_native_tags(input.text, "text")?;
    if let Some(html) = input.html {
        validate_source_native_tags(html, "html")?;
    }
    ensure_single_source_unsubscribe(input.text, "text")?;
    if let Some(html) = input.html {
        ensure_single_source_unsubscribe(html, "html")?;
    }

    let used_unsubscribe = |vars: &[String]| {
        vars.iter()
            .filter(|name| name.as_str() == "compliance.unsubscribe_url")
            .count()
    };
    if used_unsubscribe(&text_vars) > 1 {
        return Err(CampaignContentError::MultipleUnsubscribe { part: "text" });
    }
    if used_unsubscribe(&html_vars) > 1 {
        return Err(CampaignContentError::MultipleUnsubscribe { part: "html" });
    }

    validate_mappings(input)?;

    let mut replacements = BTreeMap::<String, Replacement>::new();
    let mut mapped_values = BTreeMap::<String, Vec<String>>::new();
    let mut recipient_fields = vec![BTreeMap::new(); input.recipients.len()];
    for variable in &all_vars {
        if variable == "compliance.unsubscribe_url" {
            // Never read or export Office's capability URL. Campaigns owns the
            // public opt-out action for this transmission.
            replacements.insert(
                variable.clone(),
                Replacement::Native(ORG_OPTOUT_TAG.to_string()),
            );
            continue;
        }

        let mut values = Vec::with_capacity(input.recipients.len());
        for (recipient_index, variables) in input.recipients.iter().enumerate() {
            let value = variables.get(variable).ok_or_else(|| {
                CampaignContentError::MissingRecipientValue {
                    recipient_index,
                    variable: variable.clone(),
                }
            })?;
            check_value_native_tags(value)?;
            if variables
                .get("compliance.unsubscribe_url")
                .is_some_and(|unsubscribe_url| unsubscribe_url.as_str() == value)
            {
                return Err(CampaignContentError::InvalidPlaceholder(format!(
                    "Office unsubscribe URL cannot be rendered through '{variable}'"
                )));
            }
            values.push(value.as_str());
        }

        if subject_vars.iter().any(|name| name == variable)
            && values
                .iter()
                .any(|value| domain::templates::contains_header_break(value))
        {
            return Err(CampaignContentError::HeaderBreak);
        }

        let common = values
            .first()
            .is_some_and(|first| values.iter().all(|value| value == first));
        if common {
            replacements.insert(variable.clone(), Replacement::Static(values[0].to_string()));
        } else {
            let mapping = input
                .mappings
                .get(variable)
                .ok_or_else(|| CampaignContentError::MissingMapping(variable.clone()))?;
            mapped_values.insert(
                variable.clone(),
                values.iter().map(|value| (*value).to_string()).collect(),
            );
            replacements.insert(
                variable.clone(),
                Replacement::Native(mapping.merge_tag.clone()),
            );
            for (recipient_index, value) in values.iter().enumerate() {
                recipient_fields[recipient_index]
                    .insert(mapping.contact_field.clone(), (*value).to_string());
            }
        }
    }

    if let Some(html) = input.html {
        validate_html_personalization(html, &replacements, &mapped_values)?;
    }

    let rendered_subject = render_part(input.subject, &replacements, RenderContext::Subject)?;
    let mut rendered_text = render_part(input.text, &replacements, RenderContext::Text)?;
    let mut rendered_html = match input.html {
        Some(html) => Some(render_part(html, &replacements, RenderContext::Html)?),
        None => Some(render_plain_text_html(
            input.text,
            &replacements,
            &mapped_values,
        )?),
    };

    if [
        rendered_subject.as_str(),
        rendered_text.as_str(),
        rendered_html.as_deref().unwrap_or(""),
    ]
    .iter()
    .any(|part| has_external_unsubscribe_link(part))
    {
        return Err(CampaignContentError::ExternalUnsubscribe);
    }

    ensure_single_output_unsubscribe(&rendered_text, "text")?;
    if !rendered_text.contains(ORG_OPTOUT_TAG) {
        append_text_unsubscribe_footer(&mut rendered_text);
    }
    ensure_single_output_unsubscribe(&rendered_text, "text")?;
    if let Some(html) = rendered_html.as_mut() {
        ensure_single_output_unsubscribe(html, "html")?;
        if !html.contains(ORG_OPTOUT_TAG) {
            let footer_offset = html_footer_insert_offset(html);
            if html_is_hidden_at(html, footer_offset) || has_hidden_css_rule(html) {
                return Err(CampaignContentError::UnsafePersonalization {
                    variable: "compliance.unsubscribe_url".into(),
                    context: "HTML footer",
                    reason: "the footer could be hidden by surrounding markup or CSS",
                });
            }
            append_html_unsubscribe_footer(html);
        }
        ensure_single_output_unsubscribe(html, "html")?;
        if unsafe_html_source(html) {
            return Err(CampaignContentError::UnsafeHtml);
        }
    }
    if domain::templates::contains_header_break(&rendered_subject) {
        return Err(CampaignContentError::HeaderBreak);
    }

    check_total_output_length(&rendered_subject, &rendered_text, rendered_html.as_deref())?;
    let allowed_tags: BTreeSet<&str> = input
        .mappings
        .values()
        .map(|mapping| mapping.merge_tag.as_str())
        .collect();
    validate_output_native_tags(&rendered_subject, &allowed_tags)?;
    validate_output_native_tags(&rendered_text, &allowed_tags)?;
    if let Some(html) = rendered_html.as_deref() {
        validate_output_native_tags(html, &allowed_tags)?;
    }
    for fields in &recipient_fields {
        for value in fields.values() {
            check_value_native_tags(value)?;
            check_content_length(value, "recipient field value")?;
        }
    }

    let content_hash = content_hash(
        &rendered_subject,
        &rendered_text,
        rendered_html.as_deref(),
        &recipient_fields,
    );
    Ok(CampaignContent {
        subject: rendered_subject,
        text: rendered_text,
        html: rendered_html,
        recipient_fields,
        content_hash,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderContext {
    Subject,
    Text,
    Html,
}

fn render_part(
    template: &str,
    replacements: &BTreeMap<String, Replacement>,
    context: RenderContext,
) -> Result<String, CampaignContentError> {
    let mut output = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find("{{") {
        let template_offset = template.len() - rest.len() + open;
        output.push_str(&rest[..open]);
        let after_open = &rest[open + 2..];
        let Some(close) = after_open.find("}}") else {
            return Err(CampaignContentError::InvalidPlaceholder(
                "unclosed '{{'".into(),
            ));
        };
        let name = after_open[..close].trim();
        let placeholder_end = template_offset + 2 + close + 2;
        if !valid_variable_name(name) {
            return Err(CampaignContentError::InvalidPlaceholder(name.to_string()));
        }
        let replacement = replacements.get(name).ok_or_else(|| {
            CampaignContentError::InvalidPlaceholder(format!("unresolved variable '{name}'"))
        })?;
        if context == RenderContext::Html {
            html_placeholder_context(template, name, template_offset, placeholder_end)?;
        }
        let rendered = match replacement {
            Replacement::Static(value) if context == RenderContext::Html => {
                domain::templates::html_escape(value)
            }
            Replacement::Static(value) => value.clone(),
            Replacement::Native(tag) => tag.clone(),
        };
        output.push_str(&rendered);
        rest = &after_open[close + 2..];
    }
    output.push_str(rest);
    if output.contains("{{") {
        return Err(CampaignContentError::InvalidPlaceholder(
            "unresolved '{{'".into(),
        ));
    }
    // Variable values are escaped in HTML text and attributes. These checks
    // also catch a dangerous scheme introduced by an Office variable value.
    if context == RenderContext::Html && unsafe_html_source(&output) {
        return Err(CampaignContentError::UnsafeHtml);
    }
    Ok(output)
}

fn render_plain_text_html(
    text: &str,
    replacements: &BTreeMap<String, Replacement>,
    mapped_values: &BTreeMap<String, Vec<String>>,
) -> Result<String, CampaignContentError> {
    let escaped_template = escape_plain_text_template(text)?;
    let mut html_replacements = replacements.clone();
    if html_replacements.contains_key("compliance.unsubscribe_url") {
        html_replacements.insert(
            "compliance.unsubscribe_url".into(),
            Replacement::Static(String::new()),
        );
    }
    validate_html_personalization(&escaped_template, &html_replacements, mapped_values)?;
    let rendered = render_part(&escaped_template, &html_replacements, RenderContext::Html)?;
    Ok(rendered
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "<br>\n"))
}

fn escape_plain_text_template(text: &str) -> Result<String, CampaignContentError> {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        output.push_str(&domain::templates::html_escape(&rest[..open]));
        let after_open = &rest[open + 2..];
        let Some(close) = after_open.find("}}") else {
            return Err(CampaignContentError::InvalidPlaceholder(
                "unclosed '{{'".into(),
            ));
        };
        let name = after_open[..close].trim();
        if !valid_variable_name(name) {
            return Err(CampaignContentError::InvalidPlaceholder(name.to_string()));
        }
        output.push_str("{{");
        output.push_str(&after_open[..close]);
        output.push_str("}}");
        rest = &after_open[close + 2..];
    }
    output.push_str(&domain::templates::html_escape(rest));
    Ok(output)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HtmlPlaceholderContext {
    Text,
    Attribute(String),
}

fn validate_html_personalization(
    template: &str,
    replacements: &BTreeMap<String, Replacement>,
    mapped_values: &BTreeMap<String, Vec<String>>,
) -> Result<(), CampaignContentError> {
    let mut offset = 0;
    while let Some(relative_open) = template[offset..].find("{{") {
        let open = offset + relative_open;
        let after_open = &template[open + 2..];
        let Some(close) = after_open.find("}}") else {
            return Err(CampaignContentError::InvalidPlaceholder(
                "unclosed '{{'".into(),
            ));
        };
        let name = after_open[..close].trim();
        let end = open + 2 + close + 2;
        let context = html_placeholder_context(template, name, open, end)?;
        let replacement = replacements.get(name).ok_or_else(|| {
            CampaignContentError::InvalidPlaceholder(format!("unresolved variable '{name}'"))
        })?;

        match replacement {
            Replacement::Static(value) => {
                check_html_control_characters(name, value, &context)?;
                if let HtmlPlaceholderContext::Attribute(attribute) = &context
                    && is_url_attribute(attribute)
                {
                    validate_url_attribute(name, value)?;
                }
            }
            Replacement::Native(tag) if name == "compliance.unsubscribe_url" => {
                if tag != ORG_OPTOUT_TAG {
                    return Err(CampaignContentError::InvalidPlaceholder(
                        "Office unsubscribe URL must become the Campaigns organization opt-out tag"
                            .into(),
                    ));
                }
                validate_visible_unsubscribe_location(template, name, open, end, &context)?;
            }
            Replacement::Native(_) => {
                let values = mapped_values.get(name).ok_or_else(|| {
                    CampaignContentError::InvalidPlaceholder(format!(
                        "mapped provider values are missing for '{name}'"
                    ))
                })?;
                for value in values {
                    validate_mapped_html_value(name, value, &context)?;
                }
            }
        }
        offset = end;
    }
    Ok(())
}

fn html_placeholder_context(
    template: &str,
    variable: &str,
    open: usize,
    end: usize,
) -> Result<HtmlPlaceholderContext, CampaignContentError> {
    if in_html_comment(template, open) {
        return Err(CampaignContentError::UnsafePersonalization {
            variable: variable.to_string(),
            context: "HTML comment",
            reason: "provider values are not rendered visibly inside comments",
        });
    }

    let prefix = &template[..open];
    let Some(tag_open) = prefix.rfind('<') else {
        return Ok(HtmlPlaceholderContext::Text);
    };
    if prefix[tag_open..].rfind('>').is_some() {
        return Ok(HtmlPlaceholderContext::Text);
    }

    let tag_prefix = &prefix[tag_open..];
    if tag_prefix.starts_with("</") || tag_prefix.starts_with("<!") || tag_prefix.starts_with("<?")
    {
        return Err(CampaignContentError::UnsafePersonalization {
            variable: variable.to_string(),
            context: "HTML markup",
            reason: "placeholders cannot construct HTML syntax",
        });
    }
    let Some(equals) = tag_prefix.rfind('=') else {
        return Err(CampaignContentError::UnsafePersonalization {
            variable: variable.to_string(),
            context: "HTML markup",
            reason: "placeholders inside tags must be quoted attribute values",
        });
    };
    let attribute_prefix = tag_prefix[..equals].trim_end();
    let attribute_name = attribute_prefix
        .chars()
        .rev()
        .take_while(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | ':')
        })
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>()
        .to_ascii_lowercase();
    if attribute_name.is_empty() {
        return Err(CampaignContentError::UnsafePersonalization {
            variable: variable.to_string(),
            context: "HTML markup",
            reason: "the containing attribute could not be identified safely",
        });
    }

    let value_prefix = tag_prefix[equals + 1..].trim_start();
    let Some(quote @ ('\'' | '"')) = value_prefix.chars().next() else {
        return Err(CampaignContentError::UnsafePersonalization {
            variable: variable.to_string(),
            context: "HTML attribute",
            reason: "provider merge values require a quoted attribute",
        });
    };
    let opening_quote_offset =
        tag_open + equals + 1 + tag_prefix[equals + 1..].len() - value_prefix.len();
    let after_opening_quote = opening_quote_offset + 1;
    if template[after_opening_quote..open].contains(quote) {
        return Err(CampaignContentError::UnsafePersonalization {
            variable: variable.to_string(),
            context: "HTML attribute",
            reason: "the placeholder is outside the quoted attribute value",
        });
    }
    let tag_end = template[end..]
        .find('>')
        .map(|relative| end + relative)
        .ok_or_else(|| {
            CampaignContentError::InvalidPlaceholder(
                "HTML start tag containing a placeholder is not closed".into(),
            )
        })?;
    if !template[end..tag_end].contains(quote) {
        return Err(CampaignContentError::InvalidPlaceholder(
            "HTML attribute containing a placeholder is not closed".into(),
        ));
    }
    Ok(HtmlPlaceholderContext::Attribute(attribute_name))
}

fn validate_mapped_html_value(
    variable: &str,
    value: &str,
    context: &HtmlPlaceholderContext,
) -> Result<(), CampaignContentError> {
    if value.chars().any(char::is_control) {
        return Err(CampaignContentError::UnsafePersonalization {
            variable: variable.to_string(),
            context: "HTML",
            reason: "control characters are not safe in provider merge values",
        });
    }
    match context {
        HtmlPlaceholderContext::Text => {
            if value
                .chars()
                .any(|character| matches!(character, '<' | '>' | '&'))
            {
                return Err(CampaignContentError::UnsafePersonalization {
                    variable: variable.to_string(),
                    context: "HTML text",
                    reason: "mapped values must not contain unescaped HTML characters",
                });
            }
        }
        HtmlPlaceholderContext::Attribute(attribute) if is_url_attribute(attribute) => {
            validate_url_attribute(variable, value)?;
        }
        HtmlPlaceholderContext::Attribute(_) => {
            if value
                .chars()
                .any(|character| matches!(character, '<' | '>' | '&' | '"' | '\''))
            {
                return Err(CampaignContentError::UnsafePersonalization {
                    variable: variable.to_string(),
                    context: "HTML attribute",
                    reason: "mapped values must not contain unescaped attribute characters",
                });
            }
        }
    }
    Ok(())
}

fn check_html_control_characters(
    variable: &str,
    value: &str,
    _context: &HtmlPlaceholderContext,
) -> Result<(), CampaignContentError> {
    if value.chars().any(char::is_control) {
        return Err(CampaignContentError::UnsafePersonalization {
            variable: variable.to_string(),
            context: "HTML",
            reason: "control characters are not allowed in HTML variable values",
        });
    }
    Ok(())
}

fn is_url_attribute(attribute: &str) -> bool {
    matches!(
        attribute,
        "href" | "src" | "action" | "formaction" | "poster" | "cite" | "background" | "xlink:href"
    )
}

fn validate_url_attribute(variable: &str, value: &str) -> Result<(), CampaignContentError> {
    let lower = value.to_ascii_lowercase();
    let valid_scheme = lower.starts_with("https://") || lower.starts_with("mailto:");
    let valid_shape = !value.is_empty()
        && value.len() <= 2048
        && !value.chars().any(|character| {
            character.is_whitespace()
                || character.is_control()
                || matches!(character, '<' | '>' | '"' | '\'')
        });
    let has_destination = if lower.starts_with("https://") {
        value[8..]
            .split(['/', '?', '#'])
            .next()
            .is_some_and(|host| !host.is_empty() && !host.starts_with('.'))
    } else if lower.starts_with("mailto:") {
        value[7..].contains('@')
    } else {
        false
    };
    if valid_scheme && valid_shape && has_destination {
        Ok(())
    } else {
        Err(CampaignContentError::UnsafePersonalization {
            variable: variable.to_string(),
            context: "URL attribute",
            reason: "only a complete HTTPS or mailto URL is allowed",
        })
    }
}

fn validate_visible_unsubscribe_location(
    template: &str,
    variable: &str,
    open: usize,
    end: usize,
    context: &HtmlPlaceholderContext,
) -> Result<(), CampaignContentError> {
    if !matches!(context, HtmlPlaceholderContext::Attribute(attribute) if attribute == "href") {
        return Err(CampaignContentError::UnsafePersonalization {
            variable: variable.to_string(),
            context: "HTML unsubscribe location",
            reason: "the Campaigns opt-out tag must be the href of a visible link",
        });
    }
    let lower = template.to_ascii_lowercase();
    let open_anchor = lower[..open].rfind("<a");
    let prior_close = lower[..open].rfind("</a");
    let Some(anchor_open) =
        open_anchor.filter(|anchor| prior_close.is_none_or(|close| *anchor > close))
    else {
        return Err(CampaignContentError::UnsafePersonalization {
            variable: variable.to_string(),
            context: "HTML unsubscribe location",
            reason: "the opt-out href must belong to a visible anchor",
        });
    };
    let anchor_tag_end = lower[anchor_open..]
        .find('>')
        .map(|relative| anchor_open + relative)
        .ok_or_else(|| {
            CampaignContentError::InvalidPlaceholder("unsubscribe anchor is not closed".into())
        })?;
    let anchor_close = lower[end..]
        .find("</a")
        .map(|relative| end + relative)
        .ok_or_else(|| {
            CampaignContentError::InvalidPlaceholder("unsubscribe anchor is not closed".into())
        })?;
    let label_start = anchor_tag_end + 1;
    let has_visible_label =
        template[label_start..anchor_close]
            .char_indices()
            .any(|(relative, character)| {
                !character.is_whitespace()
                    && !matches!(character, '<' | '>' | '&')
                    && !html_is_hidden_at(template, label_start + relative)
            });
    if !has_visible_label
        || html_is_hidden_at(template, anchor_tag_end + 1)
        || has_hidden_css_rule(template)
    {
        return Err(CampaignContentError::UnsafePersonalization {
            variable: variable.to_string(),
            context: "HTML unsubscribe location",
            reason: "hidden or empty unsubscribe links do not satisfy the visible footer requirement",
        });
    }
    Ok(())
}

fn in_html_comment(source: &str, offset: usize) -> bool {
    let prefix = &source[..offset];
    prefix
        .rfind("<!--")
        .is_some_and(|open| prefix.rfind("-->").is_none_or(|close| open > close))
}

fn html_is_hidden_at(source: &str, offset: usize) -> bool {
    let prefix = &source[..offset.min(source.len())];
    let lower = prefix.to_ascii_lowercase();
    let mut stack: Vec<(String, bool)> = Vec::new();
    let mut cursor = 0;
    while let Some(relative_open) = lower[cursor..].find('<') {
        let open = cursor + relative_open;
        if lower[open..].starts_with("<!--") {
            let Some(comment_end) = lower[open..].find("-->") else {
                return true;
            };
            cursor = open + comment_end + 3;
            continue;
        }
        let Some(relative_end) = lower[open..].find('>') else {
            break;
        };
        let end = open + relative_end;
        if end >= lower.len() {
            break;
        }
        let tag = &lower[open..=end];
        if tag.starts_with("</") {
            let name = html_tag_name(tag);
            if let Some(index) = stack.iter().rposition(|(open_name, _)| open_name == name) {
                stack.truncate(index);
            }
        } else if !tag.starts_with("<!") && !tag.starts_with("<?") {
            let name = html_tag_name(tag).to_string();
            let parent_hidden = stack.iter().any(|(_, hidden)| *hidden);
            let hidden = parent_hidden || hidden_html_tag(&name, tag);
            if !is_void_html_tag(&name) && !tag.ends_with("/>") {
                stack.push((name, hidden));
            }
        }
        cursor = end + 1;
    }
    stack.iter().any(|(_, hidden)| *hidden)
}

fn html_tag_name(tag: &str) -> &str {
    let start = usize::from(tag.starts_with('<')) + usize::from(tag.starts_with("</"));
    let bytes = tag.as_bytes();
    let mut end = start;
    while end < bytes.len()
        && (bytes[end].is_ascii_alphanumeric() || matches!(bytes[end], b'-' | b':'))
    {
        end += 1;
    }
    &tag[start..end]
}

fn hidden_html_tag(name: &str, tag: &str) -> bool {
    matches!(name, "head" | "style" | "template" | "script" | "noscript")
        || html_has_attribute(tag, "hidden")
        || html_attribute_value(tag, "aria-hidden")
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
        || html_attribute_value(tag, "style").is_some_and(|style| style_hides_content(&style))
}

fn html_has_attribute(tag: &str, name: &str) -> bool {
    let lower = tag.to_ascii_lowercase();
    let mut cursor = 0;
    while let Some(relative) = lower[cursor..].find(name) {
        let start = cursor + relative;
        let end = start + name.len();
        let before_ok = start == 0
            || lower.as_bytes()[start - 1].is_ascii_whitespace()
            || lower.as_bytes()[start - 1] == b'<';
        let after_ok = end == lower.len()
            || lower.as_bytes()[end].is_ascii_whitespace()
            || matches!(lower.as_bytes()[end], b'=' | b'>' | b'/');
        if before_ok && after_ok {
            return true;
        }
        cursor = end;
        if cursor >= lower.len() {
            break;
        }
    }
    false
}

fn html_attribute_value(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut cursor = 0;
    while let Some(relative) = lower[cursor..].find(name) {
        let start = cursor + relative;
        let end = start + name.len();
        let before_ok = start == 0
            || lower.as_bytes()[start - 1].is_ascii_whitespace()
            || lower.as_bytes()[start - 1] == b'<';
        let after_ok = end == lower.len()
            || lower.as_bytes()[end].is_ascii_whitespace()
            || matches!(lower.as_bytes()[end], b'=' | b'>' | b'/');
        if !before_ok || !after_ok {
            cursor = end;
            continue;
        }
        let mut value_start = end;
        while value_start < lower.len() && lower.as_bytes()[value_start].is_ascii_whitespace() {
            value_start += 1;
        }
        if value_start >= lower.len() || lower.as_bytes()[value_start] != b'=' {
            return None;
        }
        value_start += 1;
        while value_start < lower.len() && lower.as_bytes()[value_start].is_ascii_whitespace() {
            value_start += 1;
        }
        let quote = lower.as_bytes().get(value_start).copied();
        let (start, end) = match quote {
            Some(b'\'' | b'"') => {
                let value_start = value_start + 1;
                let end = lower[value_start..]
                    .find(char::from(quote.unwrap()))
                    .map(|relative| value_start + relative)?;
                (value_start, end)
            }
            _ => {
                let end = lower[value_start..]
                    .find(|character: char| character.is_ascii_whitespace() || character == '>')
                    .map(|relative| value_start + relative)
                    .unwrap_or(lower.len());
                (value_start, end)
            }
        };
        return Some(tag[start..end].to_string());
    }
    None
}

fn style_hides_content(style: &str) -> bool {
    let compact: String = style
        .to_ascii_lowercase()
        .chars()
        .filter(|character| !character.is_ascii_whitespace() && *character != '\'')
        .collect();
    [
        "display:none",
        "visibility:hidden",
        "content-visibility:hidden",
        "opacity:0",
        "font-size:0",
        "color:transparent",
        "clip-path:inset(100%)",
    ]
    .iter()
    .any(|needle| compact.contains(needle))
}

fn has_hidden_css_rule(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    let mut cursor = 0;
    while let Some(relative_start) = lower[cursor..].find("<style") {
        let start = cursor + relative_start;
        let Some(open_end) = lower[start..].find('>') else {
            return true;
        };
        let content_start = start + open_end + 1;
        let Some(close_rel) = lower[content_start..].find("</style") else {
            return true;
        };
        let content_end = content_start + close_rel;
        let rules = strip_css_comments(&lower[content_start..content_end]);
        let compact: String = rules
            .chars()
            .filter(|character| !character.is_ascii_whitespace() && *character != '\'')
            .collect();
        if [
            "display:none",
            "visibility:hidden",
            "content-visibility:hidden",
            "opacity:0",
            "font-size:0",
            "color:transparent",
            "clip-path:inset(100%)",
        ]
        .iter()
        .any(|needle| compact.contains(needle))
        {
            return true;
        }
        cursor = content_end + 7;
    }
    false
}

fn strip_css_comments(css: &str) -> String {
    let mut result = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        result.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("*/") else {
            return result;
        };
        rest = &after[end + 2..];
    }
    result.push_str(rest);
    result
}

fn is_void_html_tag(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

fn parse_placeholders(source: &str) -> Result<Vec<String>, CampaignContentError> {
    let mut names = Vec::new();
    let mut rest = source;
    while let Some(open) = rest.find("{{") {
        let after_open = &rest[open + 2..];
        let Some(close) = after_open.find("}}") else {
            return Err(CampaignContentError::InvalidPlaceholder(
                "unclosed '{{'".into(),
            ));
        };
        let name = after_open[..close].trim();
        if !valid_variable_name(name) {
            return Err(CampaignContentError::InvalidPlaceholder(name.to_string()));
        }
        names.push(name.to_string());
        rest = &after_open[close + 2..];
    }
    Ok(names)
}

fn valid_variable_name(name: &str) -> bool {
    fn valid_segment(segment: &str) -> bool {
        !segment.is_empty()
            && segment
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic())
            && segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
    }
    !name.is_empty() && name.split('.').all(valid_segment)
}

fn validate_mappings(input: &CampaignContentInput<'_>) -> Result<(), CampaignContentError> {
    let mut fields = BTreeSet::new();
    let mut tags = BTreeSet::new();
    for (variable, mapping) in input.mappings {
        let invalid = |reason: &str| CampaignContentError::InvalidMapping {
            variable: variable.clone(),
            reason: reason.to_string(),
        };
        if !valid_variable_name(variable) {
            return Err(invalid("Office variable name is invalid"));
        }
        if variable == "compliance.unsubscribe_url" {
            return Err(invalid(
                "Office unsubscribe URLs must never map to a contact field",
            ));
        }
        if !input
            .allowed_contact_fields
            .contains(&mapping.contact_field)
        {
            return Err(invalid(
                "contact field is not in the registered field allowlist",
            ));
        }
        if mapping.contact_field.trim().is_empty() {
            return Err(invalid("contact field label is empty"));
        }
        if !valid_merge_tag(&mapping.merge_tag) {
            return Err(invalid(
                "merge tag must have the form $[FIELD_NAME]$ and cannot be an LI tag",
            ));
        }
        if !fields.insert(mapping.contact_field.as_str()) {
            return Err(invalid("contact field is mapped more than once"));
        }
        if !tags.insert(mapping.merge_tag.as_str()) {
            return Err(invalid("native merge tag is mapped more than once"));
        }
    }
    Ok(())
}

fn valid_merge_tag(tag: &str) -> bool {
    let Some(field) = tag.strip_prefix("$[").and_then(|v| v.strip_suffix("]$")) else {
        return false;
    };
    let field = field.strip_prefix("UD:").unwrap_or(field);
    let mut chars = field.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn validate_source_native_tags(
    source: &str,
    part: &'static str,
) -> Result<(), CampaignContentError> {
    if source_optout_count(source) > 1 {
        return Err(CampaignContentError::MultipleUnsubscribe { part });
    }
    let Some(open) = source.find("$[") else {
        return Ok(());
    };
    let after = &source[open..];
    let Some(close) = after.find("]$") else {
        return Err(CampaignContentError::UnexpectedNativeMergeTag(
            "unterminated tag".into(),
        ));
    };
    let tag = &after[..close + 2];
    if tag == ORG_OPTOUT_TAG {
        return Err(CampaignContentError::UnexpectedNativeMergeTag(
            "Campaigns opt-out tag must be generated from the Office unsubscribe variable or footer".into(),
        ));
    }
    Err(CampaignContentError::UnexpectedNativeMergeTag(
        tag.to_string(),
    ))
}

fn validate_output_native_tags(
    source: &str,
    allowed_tags: &BTreeSet<&str>,
) -> Result<(), CampaignContentError> {
    let mut rest = source;
    while let Some(open) = rest.find("$[") {
        let after = &rest[open..];
        let Some(close) = after.find("]$") else {
            return Err(CampaignContentError::UnexpectedNativeMergeTag(
                "unterminated tag".into(),
            ));
        };
        let tag = &after[..close + 2];
        if tag != ORG_OPTOUT_TAG && !allowed_tags.contains(tag) {
            return Err(CampaignContentError::UnexpectedNativeMergeTag(
                tag.to_string(),
            ));
        }
        rest = &after[close + 2..];
    }
    Ok(())
}

fn check_value_native_tags(value: &str) -> Result<(), CampaignContentError> {
    if value.contains("$[") {
        return Err(CampaignContentError::UnexpectedNativeMergeTag(
            "native tags are not allowed in recipient values".into(),
        ));
    }
    Ok(())
}

fn source_optout_count(source: &str) -> usize {
    source.matches(ORG_OPTOUT_TAG).count()
}

fn ensure_single_source_unsubscribe(
    source: &str,
    part: &'static str,
) -> Result<(), CampaignContentError> {
    if source_optout_count(source) > 1 {
        return Err(CampaignContentError::MultipleUnsubscribe { part });
    }
    Ok(())
}

fn ensure_single_output_unsubscribe(
    source: &str,
    part: &'static str,
) -> Result<(), CampaignContentError> {
    if source_optout_count(source) > 1 {
        return Err(CampaignContentError::MultipleUnsubscribe { part });
    }
    Ok(())
}

fn append_text_unsubscribe_footer(text: &mut String) {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str("Unsubscribe: ");
    text.push_str(ORG_OPTOUT_TAG);
}

fn append_html_unsubscribe_footer(html: &mut String) {
    let offset = html_footer_insert_offset(html);
    let mut footer = String::new();
    if offset > 0 && !html[..offset].ends_with('\n') {
        footer.push('\n');
    }
    footer.push_str("<p><a href=\"");
    footer.push_str(ORG_OPTOUT_TAG);
    footer.push_str("\">Unsubscribe</a></p>");
    html.insert_str(offset, &footer);
}

fn html_footer_insert_offset(html: &str) -> usize {
    html.to_ascii_lowercase()
        .rfind("</body")
        .unwrap_or(html.len())
}

fn check_content_length(value: &str, part: &'static str) -> Result<(), CampaignContentError> {
    if value.chars().count() > MAX_CAMPAIGN_CONTENT_CHARS {
        return Err(CampaignContentError::ContentTooLong {
            part,
            max: MAX_CAMPAIGN_CONTENT_CHARS,
        });
    }
    Ok(())
}

fn check_total_output_length(
    subject: &str,
    text: &str,
    html: Option<&str>,
) -> Result<(), CampaignContentError> {
    let total =
        subject.chars().count() + text.chars().count() + html.map_or(0, |v| v.chars().count());
    if total > MAX_CAMPAIGN_CONTENT_CHARS {
        return Err(CampaignContentError::ContentTooLong {
            part: "transformed campaign content",
            max: MAX_CAMPAIGN_CONTENT_CHARS,
        });
    }
    Ok(())
}

fn unsafe_html_source(html: &str) -> bool {
    let lower = html.to_lowercase();
    const FORBIDDEN_TAGS: &[&str] = &[
        "<script",
        "<img",
        "<picture",
        "<iframe",
        "<object",
        "<embed",
        "<form",
        "<meta",
        "<link",
        "<base",
        "<svg",
        "<applet",
        "<frame",
        "<frameset",
        "<audio",
        "<video",
        "<source",
        "<track",
    ];
    if FORBIDDEN_TAGS.iter().any(|tag| lower.contains(tag))
        || lower.contains("javascript:")
        || lower.contains("vbscript:")
        || lower.contains("data:text/html")
        || lower.contains("srcdoc")
        || lower.contains("url(")
        || lower.contains("expression(")
        || lower.contains("behavior:")
        || lower.contains("@import")
        || lower.contains("srcset=")
        || lower.contains("&colon;")
    {
        return true;
    }
    has_event_handler_attribute(&lower)
}

fn unsafe_html_template_source(html: &str) -> bool {
    unsafe_html_source(html) || {
        let lower = html.to_lowercase();
        lower.contains("&#") || lower.contains("&colon;")
    }
}

fn has_event_handler_attribute(lower: &str) -> bool {
    let bytes = lower.as_bytes();
    let mut index = 0;
    while index + 2 < bytes.len() {
        if bytes[index] == b'o' && bytes[index + 1] == b'n' {
            let mut end = index + 2;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            if end > index + 2 {
                let mut equals = end;
                while equals < bytes.len() && bytes[equals].is_ascii_whitespace() {
                    equals += 1;
                }
                if equals < bytes.len() && bytes[equals] == b'=' {
                    return true;
                }
            }
        }
        index += 1;
    }
    false
}

fn has_external_unsubscribe_link(source: &str) -> bool {
    let lower = source.to_lowercase();
    let mut offset = 0;
    while let Some(relative) = next_http_url(&lower[offset..]) {
        let start = offset + relative;
        let end = lower[start..]
            .find(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '<' | '>'))
            .map_or(lower.len(), |relative_end| start + relative_end);
        if lower[start..end].contains("unsubscribe") {
            return true;
        }
        offset = end.max(start + 1);
        if offset >= lower.len() {
            break;
        }
    }

    // Catch a generic external href whose anchor is explicitly labelled as
    // an unsubscribe link, even when its URL has no unsubscribe path segment.
    let mut cursor = 0;
    while let Some(relative) = lower[cursor..].find("<a") {
        let start = cursor + relative;
        let Some(open_end_rel) = lower[start..].find('>') else {
            break;
        };
        let open_end = start + open_end_rel;
        let Some(close_rel) = lower[open_end..].find("</a") else {
            break;
        };
        let close = open_end + close_rel;
        let opening_tag = &lower[start..=open_end];
        let visible_label = &lower[open_end + 1..close];
        if visible_label.contains("unsubscribe")
            && let Some(href) = html_attribute_value(opening_tag, "href")
            && !is_office_unsubscribe_placeholder(&href)
            && !href.trim().eq_ignore_ascii_case(ORG_OPTOUT_TAG)
        {
            return true;
        }
        cursor = close + 3;
        if cursor >= lower.len() {
            break;
        }
    }
    if source.lines().any(|line| {
        let line = line.to_lowercase();
        line.contains("unsubscribe") && next_http_url(&line).is_some()
    }) {
        return true;
    }
    false
}

fn is_office_unsubscribe_placeholder(href: &str) -> bool {
    href.trim()
        .strip_prefix("{{")
        .and_then(|value| value.strip_suffix("}}"))
        .is_some_and(|name| name.trim() == "compliance.unsubscribe_url")
}

fn next_http_url(source: &str) -> Option<usize> {
    match (source.find("http://"), source.find("https://")) {
        (Some(http), Some(https)) => Some(http.min(https)),
        (Some(http), None) => Some(http),
        (None, Some(https)) => Some(https),
        (None, None) => None,
    }
}

fn content_hash(
    subject: &str,
    text: &str,
    html: Option<&str>,
    recipient_fields: &[BTreeMap<String, String>],
) -> String {
    fn append_field(output: &mut Vec<u8>, value: &[u8]) {
        output.extend_from_slice(&(value.len() as u64).to_be_bytes());
        output.extend_from_slice(value);
    }
    let mut canonical = b"free-campaign-content-v1\0".to_vec();
    append_field(&mut canonical, b"subject");
    append_field(&mut canonical, subject.as_bytes());
    append_field(&mut canonical, b"text");
    append_field(&mut canonical, text.as_bytes());
    append_field(&mut canonical, b"html");
    match html {
        Some(value) => {
            append_field(&mut canonical, b"present");
            append_field(&mut canonical, value.as_bytes());
        }
        None => append_field(&mut canonical, b"absent"),
    }
    append_field(
        &mut canonical,
        &(recipient_fields.len() as u64).to_be_bytes(),
    );
    for fields in recipient_fields {
        append_field(&mut canonical, &(fields.len() as u64).to_be_bytes());
        for (label, value) in fields {
            append_field(&mut canonical, label.as_bytes());
            append_field(&mut canonical, value.as_bytes());
        }
    }
    infrastructure::crypto::sha256_hex(&canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipient(values: &[(&str, &str)]) -> RecipientVariables {
        values
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    fn empty_mappings() -> BTreeMap<String, CampaignFieldMapping> {
        BTreeMap::new()
    }

    #[test]
    fn distinct_booking_urls_use_mapping_and_stay_with_their_recipient() {
        let recipients = vec![
            recipient(&[("meeting.booking_url", "https://book.example/one?x=1&y=2")]),
            recipient(&[("meeting.booking_url", "https://book.example/two?x=3&y=4")]),
        ];
        let mappings = BTreeMap::from([(
            "meeting.booking_url".to_string(),
            CampaignFieldMapping {
                contact_field: "BOOKING_URL".into(),
                merge_tag: "$[UD:BOOKING_URL]$".into(),
            },
        )]);
        let allowed = BTreeSet::from(["BOOKING_URL".to_string()]);
        let result = transform_campaign_content(&CampaignContentInput {
            subject: "Your meeting",
            text: "Choose a time: {{meeting.booking_url}}",
            html: Some("<p><a href=\"{{meeting.booking_url}}\">Choose a time</a></p>"),
            recipients: &recipients,
            mappings: &mappings,
            allowed_contact_fields: &allowed,
        })
        .unwrap();

        assert_eq!(
            result.text,
            "Choose a time: $[UD:BOOKING_URL]$\nUnsubscribe: $[LI:ORG_OPTOUT]$"
        );
        assert_eq!(
            result.html.as_deref(),
            Some(
                "<p><a href=\"$[UD:BOOKING_URL]$\">Choose a time</a></p>\n<p><a href=\"$[LI:ORG_OPTOUT]$\">Unsubscribe</a></p>"
            )
        );
        assert_eq!(
            result.recipient_fields[0]
                .get("BOOKING_URL")
                .map(String::as_str),
            Some("https://book.example/one?x=1&y=2")
        );
        assert_eq!(
            result.recipient_fields[1]
                .get("BOOKING_URL")
                .map(String::as_str),
            Some("https://book.example/two?x=3&y=4")
        );
    }

    #[test]
    fn identical_values_render_literally_and_escape_html() {
        let recipients = vec![
            recipient(&[("contact.name", "Mira & <Co>")]),
            recipient(&[("contact.name", "Mira & <Co>")]),
        ];
        let result = transform_campaign_content(&CampaignContentInput {
            subject: "Hello {{contact.name}}",
            text: "Hello {{contact.name}}",
            html: Some("<p style=\"color: navy\">{{contact.name}}</p>"),
            recipients: &recipients,
            mappings: &empty_mappings(),
            allowed_contact_fields: &BTreeSet::new(),
        })
        .unwrap();
        assert_eq!(result.subject, "Hello Mira & <Co>");
        assert_eq!(
            result.text,
            "Hello Mira & <Co>\nUnsubscribe: $[LI:ORG_OPTOUT]$"
        );
        assert_eq!(
            result.html.as_deref(),
            Some(
                "<p style=\"color: navy\">Mira &amp; &lt;Co&gt;</p>\n<p><a href=\"$[LI:ORG_OPTOUT]$\">Unsubscribe</a></p>"
            )
        );
        assert!(result.recipient_fields.iter().all(BTreeMap::is_empty));
    }

    #[test]
    fn distinct_values_without_mapping_are_rejected() {
        let recipients = vec![
            recipient(&[("contact.name", "Mira")]),
            recipient(&[("contact.name", "Jae")]),
        ];
        let error = transform_campaign_content(&CampaignContentInput {
            subject: "Hello",
            text: "Hello {{contact.name}}",
            html: None,
            recipients: &recipients,
            mappings: &empty_mappings(),
            allowed_contact_fields: &BTreeSet::new(),
        })
        .unwrap_err();
        assert_eq!(
            error,
            CampaignContentError::MissingMapping("contact.name".into())
        );
    }

    #[test]
    fn mapping_requires_allowlisted_field_and_strict_native_tag() {
        let recipients = vec![
            recipient(&[("contact.name", "Mira")]),
            recipient(&[("contact.name", "Jae")]),
        ];
        let mappings = BTreeMap::from([(
            "contact.name".to_string(),
            CampaignFieldMapping {
                contact_field: "UNVERIFIED".into(),
                merge_tag: "$[FNAME]$".into(),
            },
        )]);
        let error = transform_campaign_content(&CampaignContentInput {
            subject: "Hello",
            text: "Hello {{contact.name}}",
            html: None,
            recipients: &recipients,
            mappings: &mappings,
            allowed_contact_fields: &BTreeSet::new(),
        })
        .unwrap_err();
        assert!(matches!(error, CampaignContentError::InvalidMapping { .. }));

        let mappings = BTreeMap::from([(
            "contact.name".to_string(),
            CampaignFieldMapping {
                contact_field: "First Name".into(),
                merge_tag: "$[LI:ORG_OPTOUT]$".into(),
            },
        )]);
        let allowed = BTreeSet::from(["First Name".to_string()]);
        let error = transform_campaign_content(&CampaignContentInput {
            subject: "Hello",
            text: "Hello {{contact.name}}",
            html: None,
            recipients: &recipients,
            mappings: &mappings,
            allowed_contact_fields: &allowed,
        })
        .unwrap_err();
        assert!(matches!(error, CampaignContentError::InvalidMapping { .. }));
    }

    #[test]
    fn documented_ud_custom_tags_are_supported_without_generalizing_colon_tags_or_defaults() {
        let recipients = vec![
            recipient(&[("contact.name", "Mira")]),
            recipient(&[("contact.name", "Jae")]),
        ];
        let allowed = BTreeSet::from(["First Name".to_string()]);
        let mappings = BTreeMap::from([(
            "contact.name".to_string(),
            CampaignFieldMapping {
                contact_field: "First Name".into(),
                merge_tag: "$[UD:ACCOUNTMANAGER]$".into(),
            },
        )]);
        let rendered = transform_campaign_content(&CampaignContentInput {
            subject: "Office update",
            text: "Hello {{contact.name}}",
            html: Some("<p>Hello {{contact.name}}</p>"),
            recipients: &recipients,
            mappings: &mappings,
            allowed_contact_fields: &allowed,
        })
        .unwrap();
        assert!(rendered.text.contains("$[UD:ACCOUNTMANAGER]$"));
        assert!(rendered.html.unwrap().contains("$[UD:ACCOUNTMANAGER]$"));
        assert_eq!(rendered.recipient_fields[0]["First Name"], "Mira");
        assert_eq!(rendered.recipient_fields[1]["First Name"], "Jae");

        for merge_tag in [
            "$[UD:ACCOUNT-MANAGER]$",
            "$[UD:ACCOUNTMANAGER|fallback]$",
            "$[CRM:CONTACTOWNER]$",
            "$[LI:ORG_OPTOUT]$",
        ] {
            let invalid = BTreeMap::from([(
                "contact.name".to_string(),
                CampaignFieldMapping {
                    contact_field: "First Name".into(),
                    merge_tag: merge_tag.into(),
                },
            )]);
            let error = transform_campaign_content(&CampaignContentInput {
                subject: "Office update",
                text: "Hello {{contact.name}}",
                html: Some("<p>Hello {{contact.name}}</p>"),
                recipients: &recipients,
                mappings: &invalid,
                allowed_contact_fields: &allowed,
            })
            .unwrap_err();
            assert!(
                matches!(error, CampaignContentError::InvalidMapping { .. }),
                "unexpectedly accepted {merge_tag}"
            );
        }
    }

    #[test]
    fn office_unsubscribe_url_is_replaced_by_one_native_tag_per_body_and_never_exported() {
        let recipients = vec![
            recipient(&[(
                "compliance.unsubscribe_url",
                "https://office.example/unsubscribe/a",
            )]),
            recipient(&[(
                "compliance.unsubscribe_url",
                "https://office.example/unsubscribe/b",
            )]),
        ];
        let result = transform_campaign_content(&CampaignContentInput {
            subject: "Update",
            text: "Hello\n{{compliance.unsubscribe_url}}",
            html: Some(
                "<p>Hello</p><p><a href=\"{{compliance.unsubscribe_url}}\">Unsubscribe</a></p>",
            ),
            recipients: &recipients,
            mappings: &empty_mappings(),
            allowed_contact_fields: &BTreeSet::new(),
        })
        .unwrap();
        assert_eq!(result.text.matches(ORG_OPTOUT_TAG).count(), 1);
        assert_eq!(
            result
                .html
                .as_deref()
                .unwrap()
                .matches(ORG_OPTOUT_TAG)
                .count(),
            1
        );
        assert!(result.recipient_fields.iter().all(BTreeMap::is_empty));
        assert!(!result.text.contains("office.example"));
        assert!(!result.html.as_deref().unwrap().contains("office.example"));
    }

    #[test]
    fn subject_header_breaks_are_rejected() {
        let recipients = vec![recipient(&[])];
        let error = transform_campaign_content(&CampaignContentInput {
            subject: "hello\r\nBcc: attacker@example.com",
            text: "body",
            html: None,
            recipients: &recipients,
            mappings: &empty_mappings(),
            allowed_contact_fields: &BTreeSet::new(),
        })
        .unwrap_err();
        assert_eq!(error, CampaignContentError::HeaderBreak);
    }

    #[test]
    fn malicious_common_value_is_escaped_and_cannot_add_markup() {
        let recipients = vec![
            recipient(&[("contact.name", "</p><script>alert(1)</script>")]),
            recipient(&[("contact.name", "</p><script>alert(1)</script>")]),
        ];
        let result = transform_campaign_content(&CampaignContentInput {
            subject: "Hello",
            text: "{{contact.name}}",
            html: Some("<p>{{contact.name}}</p>"),
            recipients: &recipients,
            mappings: &empty_mappings(),
            allowed_contact_fields: &BTreeSet::new(),
        })
        .unwrap();
        let html = result.html.unwrap();
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("&lt;/p&gt;"));
    }

    #[test]
    fn rejects_hardcoded_external_unsubscribe_and_multiple_native_optouts() {
        let recipients = vec![recipient(&[])];
        let external = transform_campaign_content(&CampaignContentInput {
            subject: "Update",
            text: "Unsubscribe: https://office.example/public/unsubscribe/token",
            html: None,
            recipients: &recipients,
            mappings: &empty_mappings(),
            allowed_contact_fields: &BTreeSet::new(),
        });
        assert_eq!(external, Err(CampaignContentError::ExternalUnsubscribe));

        let external_template_link = transform_campaign_content(&CampaignContentInput {
            subject: "Update",
            text: "Manage preferences",
            html: Some("<a href=\"{{custom.preferences_url}}\">Unsubscribe</a>"),
            recipients: &[recipient(&[(
                "custom.preferences_url",
                "https://preferences.example.test/",
            )])],
            mappings: &empty_mappings(),
            allowed_contact_fields: &BTreeSet::new(),
        });
        assert_eq!(
            external_template_link,
            Err(CampaignContentError::ExternalUnsubscribe)
        );

        let multiple = transform_campaign_content(&CampaignContentInput {
            subject: "Update",
            text: &format!("One {ORG_OPTOUT_TAG}; two {ORG_OPTOUT_TAG}"),
            html: None,
            recipients: &recipients,
            mappings: &empty_mappings(),
            allowed_contact_fields: &BTreeSet::new(),
        });
        assert_eq!(
            multiple,
            Err(CampaignContentError::MultipleUnsubscribe { part: "text" })
        );
    }

    #[test]
    fn distinct_values_cannot_bypass_html_context_or_provider_escape_checks() {
        let recipients = vec![
            recipient(&[("contact.name", "Mira <img src=x>")]),
            recipient(&[("contact.name", "Jae <img src=x>")]),
        ];
        let mappings = BTreeMap::from([(
            "contact.name".to_string(),
            CampaignFieldMapping {
                contact_field: "First Name".into(),
                merge_tag: "$[FNAME]$".into(),
            },
        )]);
        let allowed = BTreeSet::from(["First Name".to_string()]);
        let text_node = transform_campaign_content(&CampaignContentInput {
            subject: "Update",
            text: "{{contact.name}}",
            html: Some("<p>{{contact.name}}</p>"),
            recipients: &recipients,
            mappings: &mappings,
            allowed_contact_fields: &allowed,
        });
        assert!(matches!(
            text_node,
            Err(CampaignContentError::UnsafePersonalization {
                context: "HTML text",
                ..
            })
        ));

        let unquoted_attribute = transform_campaign_content(&CampaignContentInput {
            subject: "Update",
            text: "{{contact.name}}",
            html: Some("<a href={{contact.name}}>Book</a>"),
            recipients: &[
                recipient(&[("contact.name", "https://one.example/")]),
                recipient(&[("contact.name", "https://two.example/")]),
            ],
            mappings: &mappings,
            allowed_contact_fields: &allowed,
        });
        assert!(matches!(
            unquoted_attribute,
            Err(CampaignContentError::UnsafePersonalization {
                context: "HTML attribute",
                ..
            })
        ));

        let unsafe_url = transform_campaign_content(&CampaignContentInput {
            subject: "Update",
            text: "{{contact.name}}",
            html: Some("<a href=\"{{contact.name}}\">Book</a>"),
            recipients: &[
                recipient(&[("contact.name", "javascript:alert(1)")]),
                recipient(&[("contact.name", "data:text/html,bad")]),
            ],
            mappings: &mappings,
            allowed_contact_fields: &allowed,
        });
        assert!(matches!(
            unsafe_url,
            Err(CampaignContentError::UnsafePersonalization {
                context: "URL attribute",
                ..
            })
        ));

        let quoted_url_injection = transform_campaign_content(&CampaignContentInput {
            subject: "Update",
            text: "{{contact.name}}",
            html: Some("<a href=\"{{contact.name}}\">Book</a>"),
            recipients: &[
                recipient(&[(
                    "contact.name",
                    "https://one.example/\" onmouseover=\"alert(1)",
                )]),
                recipient(&[(
                    "contact.name",
                    "https://two.example/\" onmouseover=\"alert(1)",
                )]),
            ],
            mappings: &mappings,
            allowed_contact_fields: &allowed,
        });
        assert!(matches!(
            quoted_url_injection,
            Err(CampaignContentError::UnsafePersonalization {
                context: "URL attribute",
                ..
            })
        ));
    }

    #[test]
    fn unsubscribe_marker_cannot_be_hidden_in_comments_or_display_none() {
        let recipients = vec![recipient(&[(
            "compliance.unsubscribe_url",
            "https://office.example/public/unsubscribe/token",
        )])];
        let mappings = empty_mappings();
        let allowed = BTreeSet::new();
        let hidden_comment = transform_campaign_content(&CampaignContentInput {
            subject: "Update",
            text: "Body",
            html: Some("<p>Hello</p><!-- {{compliance.unsubscribe_url}} -->"),
            recipients: &recipients,
            mappings: &mappings,
            allowed_contact_fields: &allowed,
        });
        assert!(matches!(
            hidden_comment,
            Err(CampaignContentError::UnsafePersonalization { .. })
        ));

        let hidden_style = transform_campaign_content(&CampaignContentInput {
            subject: "Update",
            text: "Body",
            html: Some(
                "<div style=\"display:none\"><a href=\"{{compliance.unsubscribe_url}}\">Unsubscribe</a></div>",
            ),
            recipients: &recipients,
            mappings: &mappings,
            allowed_contact_fields: &allowed,
        });
        assert!(matches!(
            hidden_style,
            Err(CampaignContentError::UnsafePersonalization { .. })
        ));

        let hidden_footer = transform_campaign_content(&CampaignContentInput {
            subject: "Update",
            text: "Body",
            html: Some("<body style=\"display:none\"><p>Hidden</p></body>"),
            recipients: &recipients,
            mappings: &mappings,
            allowed_contact_fields: &allowed,
        });
        assert!(matches!(
            hidden_footer,
            Err(CampaignContentError::UnsafePersonalization {
                context: "HTML footer",
                ..
            })
        ));

        let hardcoded_native_comment = transform_campaign_content(&CampaignContentInput {
            subject: "Update",
            text: "Body",
            html: Some(&format!("<p>Hello</p><!-- {ORG_OPTOUT_TAG} -->")),
            recipients: &recipients,
            mappings: &mappings,
            allowed_contact_fields: &allowed,
        });
        assert!(hardcoded_native_comment.is_err());
    }

    #[test]
    fn plain_text_office_template_gets_escaped_html_and_a_visible_native_footer() {
        let recipients = vec![
            recipient(&[("meeting.booking_url", "https://book.example/one")]),
            recipient(&[("meeting.booking_url", "https://book.example/two")]),
        ];
        let mappings = BTreeMap::from([(
            "meeting.booking_url".to_string(),
            CampaignFieldMapping {
                contact_field: "Booking URL".into(),
                merge_tag: "$[UD:BOOKING_URL]$".into(),
            },
        )]);
        let allowed = BTreeSet::from(["Booking URL".to_string()]);
        let result = transform_campaign_content(&CampaignContentInput {
            subject: "Office update",
            text: "Hello & <friend>\nBook: {{meeting.booking_url}}",
            html: None,
            recipients: &recipients,
            mappings: &mappings,
            allowed_contact_fields: &allowed,
        })
        .unwrap();
        assert_eq!(
            result.text,
            "Hello & <friend>\nBook: $[UD:BOOKING_URL]$\nUnsubscribe: $[LI:ORG_OPTOUT]$"
        );
        let html = result.html.unwrap();
        assert!(html.starts_with("Hello &amp; &lt;friend&gt;<br>\nBook: $[UD:BOOKING_URL]$"));
        assert_eq!(html.matches(ORG_OPTOUT_TAG).count(), 1);
        assert!(html.contains("href=\"$[LI:ORG_OPTOUT]$\">Unsubscribe</a>"));
        assert!(result.recipient_fields[0].contains_key("Booking URL"));
        assert_eq!(
            result.recipient_fields[0]["Booking URL"],
            "https://book.example/one"
        );
        assert_eq!(
            result.recipient_fields[1]["Booking URL"],
            "https://book.example/two"
        );
    }
}
