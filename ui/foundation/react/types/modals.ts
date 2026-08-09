export const Modals = {
  AddKey: "add-key",
  RemoveKey: "remove-key",
  QRConnect: "qr-connect",
  ConfirmSend: "confirm-send",
  ConfirmAccount: "confirm-account",
  SignWithDesktop: "sign-with-desktop",
  SignWithDesktopFromNativeCamera: "sign-with-desktop-from-native-camera",
  ConfirmSwap: "confirm-swap",
  RenewSession: "renew-session",
  ProSwapEditTPSL: "pro-edit-tpsl",
  ProSwapEditedSL: "pro-edited-sl",
  ActivityTransfer: "activity-transfer",
  ActivityConvert: "activity-convert",
  WalletSelector: "wallet-selector",
  Authenticate: "authenticate",
  EditUsername: "edit-username",
  BridgeWithdraw: "bridge-withdraw",
  BridgeDeposit: "bridge-deposit",
  AddressWarning: "address-warning",
  WindingDown: "winding-down",
  EditCommissionRate: "edit-commission-rate",
  PerpsCloseOrder: "perps-close-order",
  PerpsCloseAll: "perps-close-all",
  PerpsClosePosition: "perps-close-position",
  ActivateAccount: "activate-account",
  VaultAddLiquidity: "vault-add-liquidity",
  VaultWithdrawLiquidity: "vault-withdraw-liquidity",
  VaultWithdrawLiquidityWithPenalty: "vault-withdraw-liquidity-with-penalty",
  PerpsMarginMode: "perps-margin-mode",
  PerpsAdjustLeverage: "perps-adjust-leverage",
  FeeTiers: "fee-tiers",
  DestinationWallet: "destination-wallet",
  AdjustSlippage: "adjust-slippage",
  PnlShare: "pnl-share",
  PointsShare: "points-share",
};

export type ModalRef = {
  triggerOnClose: () => void;
};

export type ModalDefinition = {
  component: React.LazyExoticComponent<React.ForwardRefExoticComponent<any>>;
  options?: {
    header?: string;
    disableClosing?: boolean;
  };
};
