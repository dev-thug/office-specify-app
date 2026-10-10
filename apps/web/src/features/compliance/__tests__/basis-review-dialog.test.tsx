import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { NextIntlClientProvider } from 'next-intl';
import { afterEach, expect, it, vi } from 'vitest';
import { BasisReviewDialog } from '../basis-review-dialog';
import common from '../../../../messages/en/common.json';
import compliance from '../../../../messages/en/compliance.json';

afterEach(() => vi.unstubAllGlobals());

it('offers only supported review fields and sends the strict decision contract', async () => {
  const calls: RequestInit[] = [];
  vi.stubGlobal('fetch', vi.fn(async (_url, init: RequestInit) => {
    calls.push(init);
    const body = JSON.parse(String(init.body));
    if ('reason' in body) return new Response('{}', { status: 422 });
    return new Response(JSON.stringify({ data: { status: 'valid' } }), { status: 200 });
  }));
  const close = vi.fn();
  render(<NextIntlClientProvider locale="en" messages={{ common, compliance }}>
    <QueryClientProvider client={new QueryClient({ defaultOptions: { mutations: { retry: false } } })}>
      <BasisReviewDialog basis={{ id: 'basis-1', email_address_id: 'email-1', email_address: 'test@example.test', purpose: 'marketing', jurisdiction: 'default', basis_type: 'explicit_consent', status: 'pending', evidence_ref: 'user-request:test', collected_at: '2026-10-10T00:00:00Z', reviewed_by: null, reviewed_at: null, valid_until: null, revoked_at: null, version: 1, created_at: '2026-10-10T00:00:00Z' }} onOpenChange={close} />
    </QueryClientProvider>
  </NextIntlClientProvider>);
  expect(screen.queryByLabelText(/Review note/)).toBeNull();
  fireEvent.click(screen.getByRole('button', { name: 'Save review' }));
  await waitFor(() => expect(close).toHaveBeenCalledWith(false));
  expect(JSON.parse(String(calls[0]!.body))).toEqual({ decision: 'valid' });
});
