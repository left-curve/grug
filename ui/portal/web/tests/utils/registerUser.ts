import type { Page } from "@playwright/test";
import { DEFAULT_MOCK_WALLET_NAME } from "../mocks/eip6963";
import { injectMockWallet, type WalletInjectionOptions } from "./injectWallet";
import { message } from "./messages";

export type RegisterUserOptions = WalletInjectionOptions;

const registrationLabels = {
  activateAccount: message("signup.deposit.title"),
  connectWallet: message("common.connectWallet"),
  continue: message("common.continue"),
  doThisLater: message("signup.doThisLater"),
  usernamePrefix: `${message("common.account")} #`,
  usernamesFound: message("signin.usernamesFound"),
};

export async function dismissActivateAccountModal(page: Page, timeout = 2_000): Promise<void> {
  const modalText = {
    button: registrationLabels.doThisLater,
    heading: registrationLabels.activateAccount,
  };

  const isVisible = await page
    .waitForFunction(
      (labels) => {
        const normalizeText = (value: string | null) => value?.replace(/\s+/g, " ").trim() ?? "";
        const isVisible = (element: Element) => {
          const style = window.getComputedStyle(element);
          const rect = element.getBoundingClientRect();
          return (
            style.visibility !== "hidden" &&
            style.display !== "none" &&
            rect.width > 0 &&
            rect.height > 0
          );
        };

        return Array.from(document.querySelectorAll("h1,h2,h3,h4,h5,h6")).some(
          (element) => isVisible(element) && normalizeText(element.textContent) === labels.heading,
        );
      },
      modalText,
      { timeout },
    )
    .then(() => true)
    .catch(() => false);

  if (!isVisible) {
    return;
  }

  await page.waitForFunction(
    (labels) => {
      const normalizeText = (value: string | null) => value?.replace(/\s+/g, " ").trim() ?? "";
      const isVisible = (element: Element) => {
        const style = window.getComputedStyle(element);
        const rect = element.getBoundingClientRect();
        return (
          style.visibility !== "hidden" &&
          style.display !== "none" &&
          rect.width > 0 &&
          rect.height > 0
        );
      };

      const hasHeading = Array.from(document.querySelectorAll("h1,h2,h3,h4,h5,h6")).some(
        (element) => isVisible(element) && normalizeText(element.textContent) === labels.heading,
      );
      if (!hasHeading) return true;

      const button = Array.from(document.querySelectorAll("button")).find(
        (element) => isVisible(element) && normalizeText(element.textContent) === labels.button,
      );

      if (!button) return false;
      button.click();
      return true;
    },
    modalText,
    { timeout: 10_000 },
  );
  await page.waitForFunction(
    (labels) => {
      const normalizeText = (value: string | null) => value?.replace(/\s+/g, " ").trim() ?? "";
      const isVisible = (element: Element) => {
        const style = window.getComputedStyle(element);
        const rect = element.getBoundingClientRect();
        return (
          style.visibility !== "hidden" &&
          style.display !== "none" &&
          rect.width > 0 &&
          rect.height > 0
        );
      };

      return !Array.from(document.querySelectorAll("h1,h2,h3,h4,h5,h6")).some(
        (element) => isVisible(element) && normalizeText(element.textContent) === labels.heading,
      );
    },
    modalText,
    { timeout: 10_000 },
  );
}

/**
 * Registers a new user using the test EIP-6963 wallet.
 * This utility handles the full registration flow:
 * 1. Injects the mock wallet
 * 2. Navigates to the app
 * 3. Opens the auth modal
 * 4. Connects with the mock wallet
 * 5. Creates account or picks an existing one
 * 6. Dismisses any post-login modals
 *
 * After this function completes, the user is logged in.
 */
export async function registerUser(page: Page, options: RegisterUserOptions = {}): Promise<void> {
  // Inject the mock wallet before navigation
  await injectMockWallet(page, options);

  // Navigate to the app
  await page.goto("/");

  // Click on login button in the header
  await page.locator("[dng-connect-button]").click();

  // Continue from the welcome screen.
  await page.getByText(registrationLabels.connectWallet, { exact: true }).click();

  // Select the injected wallet from the wallet list.
  await page.getByText(options.walletName ?? DEFAULT_MOCK_WALLET_NAME).click();

  // After wallet authentication, the flow shows either:
  // 1) "create-account" screen (new wallet) with a continue button, or
  // 2) "account-picker" screen (existing wallet) with username list.
  const modal = page.locator(".fixed.z-\\[60\\]");
  const continueButton = modal.getByRole("button", { name: registrationLabels.continue });
  const usernamesHeading = modal.getByRole("heading", { name: registrationLabels.usernamesFound });

  const authOutcome = await Promise.race([
    continueButton.waitFor({ state: "visible", timeout: 30_000 }).then(() => "create"),
    usernamesHeading.waitFor({ state: "visible", timeout: 30_000 }).then(() => "pick"),
  ]);

  if (authOutcome === "create") {
    // New wallet — create account
    await continueButton.click();
  } else {
    // Existing wallet — select the first username (JS click to bypass viewport scroll issues)
    await modal.locator('img[alt="username"]').first().dispatchEvent("click");
  }

  // Wait for login to complete (header shows account info)
  await page
    .locator("[dng-connect-button]")
    .filter({ hasText: registrationLabels.usernamePrefix })
    .waitFor({
      state: "visible",
      timeout: 30_000,
    });

  // Auto-dismiss ActivateAccount modal whenever it appears.
  // The modal re-triggers on every full navigation (page.goto) because the
  // React ref that guards it resets on remount, so a one-time check is not enough.
  await page.addLocatorHandler(
    page.getByRole("heading", { name: registrationLabels.activateAccount }),
    async () => {
      await dismissActivateAccountModal(page, 10_000);
    },
  );
}
