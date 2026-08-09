import { AnimatePresence } from "framer-motion";
import { motion } from "framer-motion";
import { Suspense, useMemo, useRef } from "react";

import {
  Button,
  Modals,
  lazyWithRetry,
  twMerge,
  useApp,
  useMediaQuery,
} from "@left-curve/applets-kit";
import type React from "react";
import { ErrorBoundary } from "react-error-boundary";
import { Sheet, type SheetRef } from "react-modal-sheet";

import { m } from "@left-curve/foundation/paraglide/messages.js";
import { ChunkErrorFallback } from "../foundation/ChunkErrorFallback";

export type ModalRef = {
  triggerOnClose: () => void;
};

const modals: Record<(typeof Modals)[keyof typeof Modals], ModalDefinition> = {
  [Modals.AddKey]: {
    component: lazyWithRetry(() =>
      import("./AddKeyModal").then(({ AddKeyModal }) => ({ default: AddKeyModal })),
    ),
  },
  [Modals.RemoveKey]: {
    component: lazyWithRetry(() =>
      import("./RemoveKey").then(({ RemoveKey }) => ({ default: RemoveKey })),
    ),
  },
  [Modals.QRConnect]: {
    component: lazyWithRetry(() =>
      import("./QRConnect").then(({ QRConnect }) => ({ default: QRConnect })),
    ),
  },
  [Modals.ConfirmSend]: {
    component: lazyWithRetry(() =>
      import("./ConfirmSend").then(({ ConfirmSend }) => ({ default: ConfirmSend })),
    ),
  },
  [Modals.ConfirmAccount]: {
    component: lazyWithRetry(() =>
      import("./ConfirmAccount").then(({ ConfirmAccount }) => ({
        default: ConfirmAccount,
      })),
    ),
  },
  [Modals.SignWithDesktop]: {
    component: lazyWithRetry(() =>
      import("./SignWithDesktop").then(({ SignWithDesktop }) => ({
        default: SignWithDesktop,
      })),
    ),
    options: {
      header: m["common.signin"](),
    },
  },
  [Modals.SignWithDesktopFromNativeCamera]: {
    component: lazyWithRetry(() =>
      import("./SignWithDesktopFromNativeCamera").then(({ SignWithDesktopFromNativeCamera }) => ({
        default: SignWithDesktopFromNativeCamera,
      })),
    ),
    options: {
      header: m["common.signin"](),
    },
  },
  [Modals.ConfirmSwap]: {
    component: lazyWithRetry(() =>
      import("./ConfirmSwap").then(({ ConfirmSwap }) => ({
        default: ConfirmSwap,
      })),
    ),
    options: {
      header: m["dex.convert.swap"](),
    },
  },
  [Modals.RenewSession]: {
    component: lazyWithRetry(() =>
      import("./RenewSession").then(({ RenewSession }) => ({
        default: RenewSession,
      })),
    ),
    options: {
      disableClosing: true,
    },
  },
  [Modals.ProSwapEditTPSL]: {
    component: lazyWithRetry(() =>
      import("./ProSwapEditTPSL").then(({ ProSwapEditTPSL }) => ({
        default: ProSwapEditTPSL,
      })),
    ),
  },
  [Modals.ProSwapEditedSL]: {
    component: lazyWithRetry(() =>
      import("./ProSwapEditedSL").then(({ ProSwapEditedSL }) => ({
        default: ProSwapEditedSL,
      })),
    ),
  },
  [Modals.ActivityTransfer]: {
    component: lazyWithRetry(() =>
      import("./activities/ActivityTransferModal").then(({ ActivityTransferModal }) => ({
        default: ActivityTransferModal,
      })),
    ),
  },
  [Modals.ActivityConvert]: {
    component: lazyWithRetry(() =>
      import("./activities/ActivityConvertModal").then(({ ActivityConvertModal }) => ({
        default: ActivityConvertModal,
      })),
    ),
  },
  [Modals.WalletSelector]: {
    component: lazyWithRetry(() =>
      import("./WalletSelector").then(({ WalletSelector }) => ({
        default: WalletSelector,
      })),
    ),
  },
  [Modals.Authenticate]: {
    component: lazyWithRetry(() =>
      import("./Authenticate").then(({ Authenticate }) => ({
        default: Authenticate,
      })),
    ),
    options: {
      fullScreen: true,
    },
  },
  [Modals.EditUsername]: {
    component: lazyWithRetry(() =>
      import("./EditUsername").then(({ EditUsername }) => ({
        default: EditUsername,
      })),
    ),
    options: {
      disableClosing: true,
    },
  },
  [Modals.BridgeWithdraw]: {
    component: lazyWithRetry(() =>
      import("./BridgeWithdraw").then(({ BridgeWithdraw }) => ({
        default: BridgeWithdraw,
      })),
    ),
    options: {
      disableClosing: true,
    },
  },
  [Modals.BridgeDeposit]: {
    component: lazyWithRetry(() =>
      import("./BridgeDeposit").then(({ BridgeDeposit }) => ({
        default: BridgeDeposit,
      })),
    ),
    options: {
      disableClosing: true,
    },
  },
  [Modals.AddressWarning]: {
    component: lazyWithRetry(() =>
      import("./AddressWarning").then(({ AddressWarning }) => ({
        default: AddressWarning,
      })),
    ),
  },
  [Modals.WindingDown]: {
    component: lazyWithRetry(() =>
      import("./WindingDown").then(({ WindingDown }) => ({
        default: WindingDown,
      })),
    ),
  },
  [Modals.EditCommissionRate]: {
    component: lazyWithRetry(() =>
      import("./EditCommissionRate").then(({ EditCommissionRate }) => ({
        default: EditCommissionRate,
      })),
    ),
  },
  [Modals.PerpsCloseOrder]: {
    component: lazyWithRetry(() =>
      import("./PerpsCloseOrder").then(({ PerpsCloseOrder }) => ({
        default: PerpsCloseOrder,
      })),
    ),
  },
  [Modals.PerpsCloseAll]: {
    component: lazyWithRetry(() =>
      import("./PerpsCloseAll").then(({ PerpsCloseAll }) => ({
        default: PerpsCloseAll,
      })),
    ),
  },
  [Modals.PerpsClosePosition]: {
    component: lazyWithRetry(() =>
      import("./PerpsClosePosition").then(({ PerpsClosePosition }) => ({
        default: PerpsClosePosition,
      })),
    ),
  },
  [Modals.ActivateAccount]: {
    component: lazyWithRetry(() =>
      import("./ActivateAccount").then(({ ActivateAccount }) => ({
        default: ActivateAccount,
      })),
    ),
  },
  [Modals.VaultAddLiquidity]: {
    component: lazyWithRetry(() =>
      import("./VaultAddLiquidity").then(({ VaultAddLiquidity }) => ({
        default: VaultAddLiquidity,
      })),
    ),
  },
  [Modals.VaultWithdrawLiquidity]: {
    component: lazyWithRetry(() =>
      import("./VaultWithdrawLiquidity").then(({ VaultWithdrawLiquidity }) => ({
        default: VaultWithdrawLiquidity,
      })),
    ),
  },
  [Modals.VaultWithdrawLiquidityWithPenalty]: {
    component: lazyWithRetry(() =>
      import("./VaultWithdrawLiquidityWithPenalty").then(
        ({ VaultWithdrawLiquidityWithPenalty }) => ({
          default: VaultWithdrawLiquidityWithPenalty,
        }),
      ),
    ),
  },
  [Modals.PerpsMarginMode]: {
    component: lazyWithRetry(() =>
      import("./PerpsMarginMode").then(({ PerpsMarginMode }) => ({
        default: PerpsMarginMode,
      })),
    ),
  },
  [Modals.PerpsAdjustLeverage]: {
    component: lazyWithRetry(() =>
      import("./PerpsAdjustLeverage").then(({ PerpsAdjustLeverage }) => ({
        default: PerpsAdjustLeverage,
      })),
    ),
  },
  [Modals.FeeTiers]: {
    component: lazyWithRetry(() =>
      import("./FeeTiers").then(({ FeeTiers }) => ({
        default: FeeTiers,
      })),
    ),
  },
  [Modals.DestinationWallet]: {
    component: lazyWithRetry(() =>
      import("./DestinationWallet").then(({ DestinationWallet }) => ({
        default: DestinationWallet,
      })),
    ),
  },
  [Modals.AdjustSlippage]: {
    component: lazyWithRetry(() =>
      import("./AdjustSlippage").then(({ AdjustSlippage }) => ({
        default: AdjustSlippage,
      })),
    ),
  },
  [Modals.PnlShare]: {
    component: lazyWithRetry(() =>
      import("./pnl-share/PnlShare").then(({ PnlShare }) => ({
        default: PnlShare,
      })),
    ),
  },
  [Modals.PointsShare]: {
    component: lazyWithRetry(() =>
      import("./points-share/PointsShare").then(({ PointsShare }) => ({
        default: PointsShare,
      })),
    ),
  },
};

type ModalDefinition = {
  component: React.LazyExoticComponent<React.ComponentType<any>>;
  options?: {
    header?: string;
    disableClosing?: boolean;
    fullScreen?: boolean;
  };
};

export const RootModal: React.FC = () => {
  const { modal, hideModal } = useApp();
  const { isMd } = useMediaQuery();

  const sheetRef = useRef<SheetRef>(null);
  const outlineRef = useRef<HTMLDivElement>(null);
  const modalRef = useRef<{ triggerOnClose: () => void } | null>(null);

  const { modal: activeModal, props: modalProps } = modal;

  const { component: Modal, options = {} } =
    useMemo(() => modals[activeModal as keyof typeof modals], [activeModal, sheetRef]) || {};

  const closeModal = () => {
    hideModal();
    modalRef.current?.triggerOnClose();
  };

  if (!activeModal)
    return <AnimatePresence initial={false} mode="wait" onExitComplete={() => null} />;

  if (!isMd) {
    return (
      <Sheet
        disableDrag={options.disableClosing}
        ref={sheetRef}
        isOpen={!!activeModal}
        onClose={closeModal}
        detent={options.fullScreen ? "full-height" : "content-height"}
        rootId="root"
      >
        <Sheet.Container
          className={twMerge(
            "!bg-surface-primary-rice !shadow-none",
            options.fullScreen ? "!rounded-none" : "!rounded-t-2xl !max-h-[90vh]",
          )}
        >
          <Sheet.Header>
            {options.header ? (
              <div className="flex items-center justify-between w-full">
                <Button variant="link" onClick={hideModal}>
                  {m["common.cancel"]()}
                </Button>
                <p className="mt-1 text-ink-tertiary-500 font-semibold">{options.header}</p>
                <div className="w-[66px]" />
              </div>
            ) : null}
          </Sheet.Header>
          <Sheet.Content className="!overflow-y-auto">
            <Sheet.Scroller>
              <ErrorBoundary FallbackComponent={ChunkErrorFallback} onReset={closeModal}>
                <Suspense>
                  <div className="pb-[env(safe-area-inset-bottom,20px)]">
                    <Modal ref={modalRef} {...modalProps} />
                  </div>
                </Suspense>
              </ErrorBoundary>
            </Sheet.Scroller>
          </Sheet.Content>
        </Sheet.Container>
        <Sheet.Backdrop onTap={() => !options.disableClosing && closeModal()} />
      </Sheet>
    );
  }

  return (
    <AnimatePresence initial={false} mode="wait" onExitComplete={() => null}>
      <motion.div
        ref={outlineRef}
        onClick={(e) => {
          if (e.target === outlineRef.current && !options.disableClosing) closeModal();
        }}
        className="bg-primitives-gray-light-900/50 w-screen h-screen fixed top-0 z-[60] flex items-center justify-center p-4"
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0 }}
      >
        <ErrorBoundary FallbackComponent={ChunkErrorFallback} onReset={closeModal}>
          <Suspense>
            <Modal ref={modalRef} {...modalProps} />
          </Suspense>
        </ErrorBoundary>
      </motion.div>
    </AnimatePresence>
  );
};
