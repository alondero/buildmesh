// verify-ui-opencode.mjs — drive the OpenCodeAccountCard through the
// awaitingActivation branch and capture the verification URL display.
//
// Context: { page, invoke } (Playwright + the HTTP test bridge on :2991).
// The OpenCode start_device_flow_console IPC hits the LIVE
// console.opencode.ai server, so the UI renders the real relative-path
// verification_uri — the fix prepends OPENCODE_CONSOLE_HOST so the rendered
// URL is a full https:// URL that's both openable in a browser AND
// copy/paste-able.

export default async function ({ page }) {
  // The dev profile's CDP attach lands on the existing Settings modal
  // (re-opened from the last-known tab). The title-bar icon click is
  // blocked by the modal's backdrop, so navigate by tab directly via
  // text content. `force: true` bypasses Playwright's pointer-event
  // checks; the click handler already fires on the React synthetic
  // event regardless of the CSS layer.
  const providersTab = page.locator('button').filter({ hasText: /^Providers$/ }).first();
  await providersTab.waitFor({ state: 'attached', timeout: 5_000 });
  await providersTab.click({ force: true });

  // Find the OpenCode Account card by its h4 header.
  const cardHeading = page.locator('h4:has-text("OpenCode Console")');
  await cardHeading.waitFor({ state: 'visible', timeout: 5_000 });

  // If the dance is already in flight (the previous run left the card
  // in `awaitingActivation`), skip the click and assert directly on the
  // rendered URL. Otherwise start the dance so the live IPC round-trips
  // to console.opencode.ai and the rendered URL proves the Rust
  // prepend worked end-to-end.
  const urlBlock = page.getByTestId('opencode-verification-url');
  const alreadyVisible = await urlBlock.isVisible().catch(() => false);
  if (!alreadyVisible) {
    const signInBtn = page.getByTestId('opencode-sign-in');
    await signInBtn.waitFor({ state: 'visible', timeout: 5_000 });
    await signInBtn.click();
  }

  // The awaitingActivation branch renders the verification URL as a
  // selectable text block (opencode-verification-url testid) AND as a
  // clickable anchor (opencode-verification-link). Both should be
  // present and the URL should be the full https:// form.
  await urlBlock.waitFor({ state: 'visible', timeout: 15_000 });
  const urlText = (await urlBlock.textContent()) ?? '';
  if (!urlText.startsWith('https://console.opencode.ai/')) {
    throw new Error(
      `verification URL is not a full URL after Rust prepend — got: ${JSON.stringify(urlText)}`,
    );
  }
  if (urlText.startsWith('https://console.opencode.ai/device?')) {
    // The relative-path form starts with /device — back-end prepending
    // is the load-bearing bit. This is the matching live shape.
  } else {
    throw new Error(
      `verification URL does not match the live server's relative-path contract: ${JSON.stringify(urlText)}`,
    );
  }

  // The anchor must also carry the full URL as href.
  const link = page.getByTestId('opencode-verification-link');
  await link.waitFor({ state: 'visible', timeout: 5_000 });
  const href = await link.getAttribute('href');
  if (href !== urlText) {
    throw new Error(
      `anchor href (${JSON.stringify(href)}) does not match the rendered URL (${JSON.stringify(urlText)})`,
    );
  }

  // The user_code is also surfaced — pin it stays as 4+4 chars (per RFC
  // 8628 §6.1 the live server returns dashes).
  const code = page.getByTestId('opencode-user-code');
  const codeText = (await code.textContent()) ?? '';
  if (!/^[A-Z0-9]{4}-[A-Z0-9]{4}$/.test(codeText.trim())) {
    throw new Error(
      `user_code does not match the live XXXX-XXXX shape: ${JSON.stringify(codeText)}`,
    );
  }
}
