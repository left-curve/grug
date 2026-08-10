import { Modals, twMerge, useApp, useMediaQuery, useTheme } from "@left-curve/applets-kit";
import { useAccount, useBalances, useConfig } from "@left-curve/store";
import { captureException } from "@sentry/react";
import { Outlet, createFileRoute, useRouter, useSearch } from "@tanstack/react-router";
import { useEffect, useMemo, useRef, useState } from "react";
import { ErrorBoundary } from "react-error-boundary";
import { ChunkErrorFallback } from "~/components/foundation/ChunkErrorFallback";
import { Footer } from "~/components/foundation/Footer";
import { Header } from "~/components/foundation/Header";
import { NotFound } from "~/components/foundation/NotFound";
import { StatusBadge } from "~/components/foundation/StatusBadge";
import { GeoblockBanner } from "~/components/foundation/GeoblockBanner";
import { useGeoblock } from "~/components/foundation/hooks/useGeoblock";
import { TestnetBanner } from "~/components/foundation/TestnetBanner";

import { z } from "zod";
import { Image } from "~/components/foundation/Image";
import { EXCHANGE_SHUTDOWN_NOTICE } from "~/constants";

export const Route = createFileRoute("/(app)/_app")({
  validateSearch: z.object({
    notice: z.literal(EXCHANGE_SHUTDOWN_NOTICE).optional(),
    socketId: z.string().optional(),
  }),
  component: LayoutApp,
  errorComponent: ({ error }) => {
    useEffect(() => {
      captureException(error);
    }, []);

    const { theme } = useTheme();
    const { isLg } = useMediaQuery();

    return (
      <main className="flex flex-col h-screen w-screen relative items-center justify-start overflow-x-hidden bg-surface-primary-rice text-ink-secondary-700">
        <Image
          src={theme === "dark" ? "/images/union-dark.png" : "/images/union.png"}
          alt="bg-image"
          className={twMerge(
            "drag-none select-none h-[15vh] lg:h-[20vh] w-full fixed lg:absolute bottom-0 lg:top-0 left-0 z-40 lg:z-0 rotate-180 lg:rotate-0",
            { hidden: location.pathname === "/" && !isLg },
          )}
        />
        <Header isScrolled={false} />
        <NotFound />
        <StatusBadge />
      </main>
    );
  },
});

function LayoutApp() {
  const { showModal } = useApp();
  const [isScrolled, setIsScrolled] = useState(false);
  const { isLg } = useMediaQuery();
  const isGeoblocked = useGeoblock();
  const router = useRouter();
  const { isSidebarVisible } = useApp();
  const { isConnected, userStatus, account } = useAccount();
  const { chain } = useConfig();
  const { data: balances } = useBalances({ address: account?.address });
  const modalShowed = useRef(false);

  const isProSwap = useMemo(() => {
    return router.state.location.pathname.includes("trade");
  }, [router.state.location.pathname]);

  const { notice, socketId } = useSearch({ strict: false });
  const isExchangeShutdownNotice = notice === EXCHANGE_SHUTDOWN_NOTICE;

  useEffect(() => {
    if (isExchangeShutdownNotice) showModal(Modals.WindingDown);
  }, [isExchangeShutdownNotice, showModal]);

  useEffect(() => {
    if (isExchangeShutdownNotice) return;
    if (!isConnected) modalShowed.current = false;
    if (!isConnected || modalShowed.current) return;

    const isMainnet = chain.id === "dango-1";
    const needsActivation = isMainnet
      ? userStatus && userStatus !== "active"
      : balances && Object.keys(balances).length === 0;

    if (needsActivation) {
      modalShowed.current = true;
      showModal(Modals.ActivateAccount);
    }
  }, [isConnected, userStatus, balances, chain.id, isExchangeShutdownNotice, showModal]);

  useEffect(() => {
    if (socketId) showModal(Modals.SignWithDesktopFromNativeCamera, { socketId });
    const params = new URLSearchParams(window.location.search);
    const authCallback = params.get("auth_callback");
    const ref = params.get("ref");
    if (authCallback) {
      const url = new URL(window.location.href);
      url.searchParams.delete("auth_callback");
      url.searchParams.delete("ref");
      window.history.replaceState({}, "", url.pathname + (url.search || ""));
      showModal(Modals.Authenticate, { referrer: ref ? Number.parseInt(ref, 10) : undefined });
    }
  }, []);

  const headerThreshold = isProSwap ? 1 : 70;

  useEffect(() => {
    const handleScroll = () => {
      const scrollTop = window.scrollY || 0;
      setIsScrolled(scrollTop > headerThreshold);
    };

    window.addEventListener("scroll", handleScroll);
    return () => window.removeEventListener("scroll", handleScroll);
  }, [isProSwap]);

  const { theme } = useTheme();

  const lockedY = Number(document.body.dataset.scrollLockY || 0);

  const effectiveIsScrolled = isSidebarVisible ? lockedY > headerThreshold : isScrolled;

  return (
    <main
      className={twMerge(
        "flex flex-col w-full min-h-[100svh] relative pb-[3rem] max-w-screen bg-surface-primary-rice text-ink-secondary-700",
        isProSwap ? "lg:pb-0" : "lg:pb-10",
      )}
    >
      <Image
        src={theme === "dark" ? "/images/union-dark.png" : "/images/union.png"}
        alt="bg-image"
        className="pointer-events-none drag-none select-none h-[20vh] lg:h-[20vh] w-full fixed lg:absolute bottom-0 lg:top-0 left-0 z-40 lg:z-0 rotate-180 lg:rotate-0 object-cover object-bottom"
      />
      {!isLg && isProSwap && isGeoblocked ? <GeoblockBanner /> : null}
      {!isLg ? <TestnetBanner /> : null}
      <Header isScrolled={effectiveIsScrolled} />
      <div className="flex flex-1 items-center justify-start w-full h-full relative flex-col z-30">
        <ErrorBoundary
          FallbackComponent={ChunkErrorFallback}
          resetKeys={[router.state.location.pathname]}
          onReset={() => router.invalidate()}
          onError={(error) => captureException(error)}
        >
          <Outlet />
        </ErrorBoundary>
      </div>
      <Footer />
    </main>
  );
}
