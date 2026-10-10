//! Strict parsing for RFC message identifiers returned by an authenticated
//! mailbox provider. This module extracts only Message-ID threading fields;
//! raw From and Authentication-Results values are never used as proof of
//! sender identity.

const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_MESSAGE_ID_BYTES: usize = 998;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedMessageHeaders {
    pub internet_message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
}

/// Parse only `Message-ID`, `In-Reply-To`, and `References` from a bounded
/// RFC header block. Malformed fields fail closed independently, so a bad
/// References value cannot be partially used to attach a message.
pub fn parse_message_headers(raw: &str) -> ParsedMessageHeaders {
    let mut parsed = ParsedMessageHeaders::default();
    let Some(fields) = unfold_fields(raw) else {
        return parsed;
    };

    let message_ids: Vec<_> = fields
        .iter()
        .filter(|(name, _)| name == "message-id")
        .collect();
    if let [(_, value)] = message_ids.as_slice()
        && let Some(mut ids) = parse_message_id_list(value)
        && ids.len() == 1
    {
        parsed.internet_message_id = ids.pop();
    }

    let in_reply_to: Vec<_> = fields
        .iter()
        .filter(|(name, _)| name == "in-reply-to")
        .collect();
    if let [(_, value)] = in_reply_to.as_slice()
        && let Some(ids) = parse_message_id_list(value)
    {
        // If a sender emits several parent IDs, the last is the closest
        // ancestor and therefore the useful exact target for attribution.
        parsed.in_reply_to = ids.last().cloned();
    }

    let reference_fields: Vec<_> = fields
        .iter()
        .filter(|(name, _)| name == "references")
        .collect();
    if !reference_fields.is_empty() {
        let mut references = Vec::new();
        let mut valid = true;
        for (_, value) in reference_fields {
            match parse_message_id_list(value) {
                Some(ids) => references.extend(ids),
                None => {
                    valid = false;
                    break;
                }
            }
        }
        if valid {
            for id in references {
                if !parsed.references.contains(&id) {
                    parsed.references.push(id);
                }
            }
        }
    }

    parsed
}

fn unfold_fields(raw: &str) -> Option<Vec<(String, String)>> {
    if raw.len() > MAX_HEADER_BYTES || raw.as_bytes().contains(&0) {
        return None;
    }
    let without_crlf = raw.replace("\r\n", "");
    if without_crlf.contains('\r') || without_crlf.contains('\n') {
        return None;
    }
    let unfolded = raw.replace("\r\n", "\n");
    if unfolded.contains('\r') {
        return None;
    }

    let mut fields = Vec::new();
    let mut current: Option<(String, String)> = None;
    for line in unfolded.split('\n') {
        if line.is_empty() {
            if let Some(field) = current.take() {
                fields.push(field);
            }
            break;
        }
        if line.starts_with([' ', '\t']) {
            let (_, value) = current.as_mut()?;
            value.push(' ');
            value.push_str(line.trim_start_matches([' ', '\t']));
            continue;
        }

        if let Some(field) = current.take() {
            fields.push(field);
        }
        let (name, value) = line.split_once(':')?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || value
                .bytes()
                .any(|byte| (byte < b' ' && byte != b'\t') || byte == 0x7f)
        {
            return None;
        }
        current = Some((
            name.to_ascii_lowercase(),
            value.trim_start_matches([' ', '\t']).to_owned(),
        ));
    }
    if let Some(field) = current {
        fields.push(field);
    }
    Some(fields)
}

pub fn parse_message_id_list(value: &str) -> Option<Vec<String>> {
    if value.len() > MAX_HEADER_BYTES {
        return None;
    }
    let bytes = value.as_bytes();
    let mut ids = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b',') {
            pos += 1;
        }
        if pos == bytes.len() {
            break;
        }
        if bytes[pos] != b'<' {
            return None;
        }
        let start = pos + 1;
        let relative_end = bytes[start..].iter().position(|byte| *byte == b'>')?;
        let end = start + relative_end;
        let id = &value[start..end];
        if !valid_message_id(id) {
            return None;
        }
        ids.push(format!("<{id}>"));
        pos = end + 1;
        if pos < bytes.len() && !matches!(bytes[pos], b' ' | b'\t' | b',') {
            return None;
        }
    }
    if ids.is_empty() { None } else { Some(ids) }
}

/// Validate and canonicalize one angle-bracketed RFC Message-ID.
pub fn parse_single_message_id(value: &str) -> Option<String> {
    let mut ids = parse_message_id_list(value)?;
    if ids.len() == 1 { ids.pop() } else { None }
}

fn valid_message_id(id: &str) -> bool {
    if id.is_empty() || id.len() > MAX_MESSAGE_ID_BYTES || !id.is_ascii() {
        return false;
    }
    let Some((left, right)) = id.split_once('@') else {
        return false;
    };
    if left.is_empty() || right.is_empty() || right.contains('@') {
        return false;
    }
    valid_dot_atom(left) && valid_dot_atom(right)
}

fn valid_dot_atom(value: &str) -> bool {
    const ATOM: &str = "!#$%&'*+-/=?^_`{|}~";
    value.split('.').all(|part| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || ATOM.as_bytes().contains(&byte))
    })
}

#[cfg(test)]
mod tests {
    use super::parse_message_headers;

    #[test]
    fn extracts_rfc_message_ids_from_folded_and_repeated_references() {
        let headers = parse_message_headers(
            "Message-ID: <reply@sender.example>\r\n\
             In-Reply-To: <outbound@office.example>\r\n\
             References: <root@office.example>\r\n\
             \t<outbound@office.example>\r\n\
             rEfErEnCeS: <older@office.example>\r\n\
             Authentication-Results: sender.example; dkim=pass\r\n\r\n",
        );

        assert_eq!(
            headers.internet_message_id.as_deref(),
            Some("<reply@sender.example>")
        );
        assert_eq!(
            headers.in_reply_to.as_deref(),
            Some("<outbound@office.example>")
        );
        assert_eq!(
            headers.references,
            vec![
                "<root@office.example>",
                "<outbound@office.example>",
                "<older@office.example>"
            ]
        );
    }

    #[test]
    fn malformed_reference_header_is_ignored_instead_of_partially_matched() {
        let headers = parse_message_headers(
            "Message-ID: <reply@sender.example>\r\n\
             References: <outbound@office.example> junk <other@office.example>\r\n\r\n",
        );

        assert_eq!(
            headers.internet_message_id.as_deref(),
            Some("<reply@sender.example>")
        );
        assert!(headers.references.is_empty());
    }

    #[test]
    fn malformed_and_duplicate_message_id_headers_fail_closed() {
        for raw in [
            "Message-ID: not-an-id\r\n\r\n",
            "Message-ID: <first@sender.example>\r\nMessage-ID: <second@sender.example>\r\n\r\n",
            "Message-ID: <first@sender.example> trailing\r\n\r\n",
        ] {
            assert!(
                parse_message_headers(raw).internet_message_id.is_none(),
                "{raw}"
            );
        }
    }

    #[test]
    fn bare_lf_header_lines_are_rejected() {
        let raw = "Message-ID: <reply@sender.example>\nReferences: <outbound@office.example>\n\n";
        assert_eq!(parse_message_headers(raw), Default::default());
    }
}
