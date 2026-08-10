import {
  twMerge,
  useAnimateOnce,
  useApp,
  useClickAway,
  useMediaQuery,
  usePreserveScroll,
} from "@left-curve/applets-kit";
import { useLocation, useNavigate } from "@tanstack/react-router";
import { useEffect, useRef } from "react";
import { useFavApplets, useSearchBar } from "@left-curve/store";

import { m } from "@left-curve/foundation/paraglide/messages.js";
import { APPLETS } from "~/constants";

import {
  IconButton,
  IconChevronDown,
  IconClose,
  IconSearch,
  ResizerContainer,
  Spinner,
  TextLoop,
} from "@left-curve/applets-kit";
import { Command } from "cmdk";
import { AnimatePresence, motion } from "framer-motion";
import { SearchItem } from "./SearchItem";

import type React from "react";
import type { AppletMetadata } from "@left-curve/store/types";
import type { SearchBarResult } from "@left-curve/store";

const SearchMenu: React.FC = () => {
  const { isLg } = useMediaQuery();
  const location = useLocation();
  const { favApplets } = useFavApplets();
  const { isSearchBarVisible, setSearchBarVisibility } = useApp();
  const { searchText, setSearchText, isLoading, searchResult, allNotFavApplets, isRefetching } =
    useSearchBar({
      applets: APPLETS,
      favApplets,
    });

  const inputRef = useRef<HTMLInputElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const isIntentionalBlurRef = useRef(false);

  useClickAway(menuRef, () => {
    if (!isLg) return;
    isIntentionalBlurRef.current = true;
    setSearchBarVisibility(false);
    setSearchText("");
  });

  useEffect(() => {
    if (!isLg) return;
    const handleKeyDown = async (e: KeyboardEvent) => {
      if (!isSearchBarVisible && e.key === "k" && e.metaKey) {
        inputRef.current?.focus();
        setSearchBarVisibility(true);
      } else if (e.metaKey || e.ctrlKey) {
        return;
      } else if (isSearchBarVisible && e.key === "Escape") {
        setSearchBarVisibility(false);
        setSearchText("");
        isIntentionalBlurRef.current = true;
        inputRef.current?.blur();
      } else if (
        !["INPUT", "TEXT", "TEXTAREA"].includes(window.document.activeElement?.nodeName || "") &&
        e.key.length === 1 &&
        /\w/i.test(e.key)
      ) {
        e.preventDefault();
        setSearchBarVisibility(true);
        inputRef.current?.focus();
        setSearchText((prev) => prev + e.key);
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [isSearchBarVisible]);

  const openMenu = () => {
    setSearchBarVisibility(true);
    inputRef.current?.focus();
  };

  const hideMenu = () => {
    setSearchBarVisibility(false);
    setSearchText("");
    isIntentionalBlurRef.current = true;
    inputRef.current?.blur();
  };

  return (
    <Command ref={menuRef} className="flex flex-col gap-4 w-full" shouldFilter={false}>
      <ResizerContainer layoutId="search-menu">
        <div
          className={twMerge(
            "flex-col bg-surface-secondary-rice rounded-md h-[44px] lg:h-auto w-full flex items-center lg:absolute relative flex-1 lg:shadow-account-card transition-all duration-300",
            !isLg && isSearchBarVisible
              ? "h-svh w-full fixed inset-0 z-[100] bg-surface-primary-rice p-4 gap-4 rounded-none"
              : "",
            isLg && location.pathname === "/" ? "lg:top-0" : "lg:top-[-22px]",
          )}
        >
          <div className="w-full gap-[10px] lg:gap-0 flex items-center">
            {!isLg && isSearchBarVisible ? (
              <IconButton variant="link" onClick={hideMenu}>
                <IconChevronDown className="rotate-90" />
              </IconButton>
            ) : null}
            <div className="flex-col bg-surface-secondary-rice shadow-account-card lg:shadow-none rounded-md w-full flex items-center">
              <motion.div className="w-full flex items-center gap-2 px-3 py-2 rounded-md">
                <IconSearch className="w-5 h-5 text-ink-tertiary-500" />
                <Command.Input
                  ref={inputRef}
                  aria-label={`${m["searchBar.placeholder.title"]()} ${m["searchBar.placeholder.apps"]()}`}
                  onValueChange={setSearchText}
                  value={searchText}
                  onBlur={(e) => {
                    if (isIntentionalBlurRef.current) {
                      isIntentionalBlurRef.current = false;
                      return;
                    }

                    if (!e.relatedTarget && isSearchBarVisible) {
                      requestAnimationFrame(() => {
                        inputRef.current?.focus();
                      });
                    }
                  }}
                  className="bg-surface-secondary-rice pt-[4px] w-full outline-none focus:outline-none placeholder:text-ink-tertiary-500"
                />

                {!isLg && searchText ? (
                  <IconClose
                    className="w-6 h-6 text-ink-tertiary-500"
                    onClick={() => setSearchText("")}
                  />
                ) : null}
              </motion.div>
              {!isSearchBarVisible && (
                <AnimatePresence mode="wait" custom={isSearchBarVisible}>
                  <motion.button
                    type="button"
                    initial={{ opacity: 0 }}
                    animate={{ opacity: 1 }}
                    exit={{
                      opacity: 0,
                      transition: { duration: 0.2 },
                    }}
                    transition={{ duration: 1 }}
                    className="flex absolute w-full h-full bg-transparent left-0 rounded-md cursor-text gap-1 items-center pl-9 pt-1 diatype-m-regular"
                    onClick={() => (isSearchBarVisible ? hideMenu() : openMenu())}
                  >
                    <span className="whitespace-nowrap">{m["searchBar.placeholder.title"]()}</span>{" "}
                    <TextLoop
                      texts={[
                        m["searchBar.placeholder.transactions"](),
                        m["searchBar.placeholder.apps"](),
                        m["searchBar.placeholder.blocks"](),
                        m["searchBar.placeholder.accounts"](),
                        m["searchBar.placeholder.usernames"](),
                        m["searchBar.placeholder.tokens"](),
                      ]}
                    />
                  </motion.button>
                </AnimatePresence>
              )}
            </div>
          </div>

          <Body
            isVisible={isSearchBarVisible}
            hideMenu={hideMenu}
            searchResult={searchResult}
            isSearching={!!searchText}
            allApplets={allNotFavApplets}
            isLoading={isLoading || isRefetching}
          />
        </div>
      </ResizerContainer>
    </Command>
  );
};

type SearchMenuBodyProps = {
  isVisible: boolean;
  isSearching: boolean;
  hideMenu: () => void;
  allApplets: AppletMetadata[];
  searchResult: SearchBarResult;
  isLoading: boolean;
};

const Body: React.FC<SearchMenuBodyProps> = ({
  isVisible,
  hideMenu,
  searchResult,
  isLoading,
  isSearching,
  allApplets,
}) => {
  const navigate = useNavigate();
  const { applets, block, txs, account, contracts, user } = searchResult;
  const scrollRef = usePreserveScroll([applets, contracts, allApplets]);
  const hasAnimated = useAnimateOnce(isVisible);

  return (
    <AnimatePresence mode="wait" custom={isVisible}>
      {isVisible && (
        <motion.div
          initial={{ height: 0 }}
          animate={{ height: "auto" }}
          exit={{ height: 0 }}
          transition={{ duration: 0.1 }}
          className="menu w-full overflow-hidden flex-1 min-h-0"
        >
          <div
            ref={scrollRef}
            className="overflow-y-auto max-h-full md:max-h-[25.15rem] lg:max-h-[25.15rem] scrollbar-thin scrollbar-thumb-outline-secondary-gray scrollbar-track-transparent"
          >
            <motion.div
              className="lg:p-1 w-full flex items-center flex-col gap-1"
              variants={{
                hidden: {},
                visible: {
                  transition: {
                    delayChildren: 0.1,
                    staggerChildren: 0.05,
                  },
                },
              }}
              initial={hasAnimated ? false : "hidden"}
              animate="visible"
            >
              <Command.List className="w-full">
                <Command.Empty>
                  {isLoading ? (
                    <div className="flex items-center justify-center w-full p-2">
                      <Spinner color="pink" size="lg" />
                    </div>
                  ) : (
                    <p className="text-ink-tertiary-500 diatype-m-regular p-2 text-center">
                      {m["searchBar.noResult"]()}
                    </p>
                  )}
                </Command.Empty>
                {applets.length ? (
                  <Command.Group heading={isSearching ? "Applets" : "Favorite Applets"}>
                    {applets.map((applet) => (
                      <Command.Item
                        key={applet.title}
                        value={applet.title}
                        className="group"
                        disabled={applet.isDisabled}
                        onSelect={
                          applet.isDisabled
                            ? undefined
                            : () => [navigate({ to: applet.path }), hideMenu()]
                        }
                      >
                        <SearchItem.Applet key={applet.title} {...applet} />
                      </Command.Item>
                    ))}
                  </Command.Group>
                ) : null}
                {!isSearching && allApplets.length ? (
                  <Command.Group heading="Applets">
                    {allApplets.map((applet) => (
                      <Command.Item
                        key={applet.title}
                        value={applet.title}
                        className="group"
                        disabled={applet.isDisabled}
                        onSelect={
                          applet.isDisabled
                            ? undefined
                            : () => [navigate({ to: applet.path }), hideMenu()]
                        }
                      >
                        <SearchItem.Applet key={applet.title} {...applet} />
                      </Command.Item>
                    ))}
                  </Command.Group>
                ) : null}
                {block ? (
                  <Command.Group heading="Block">
                    <Command.Item
                      key={block.hash}
                      value={block.hash}
                      className="group"
                      onSelect={() => [navigate({ to: `/block/${block.blockHeight}` }), hideMenu()]}
                    >
                      <SearchItem.Block height={block.blockHeight} hash={block.hash} />
                    </Command.Item>
                  </Command.Group>
                ) : null}
                {txs.length
                  ? txs.map((tx) => (
                      <Command.Group heading="Transactions" key={tx.hash}>
                        <Command.Item
                          key={tx.hash}
                          value={tx.hash}
                          className="group"
                          onSelect={() => [navigate({ to: `/tx/${tx.hash}` }), hideMenu()]}
                        >
                          <SearchItem.Transaction height={tx.blockHeight} hash={tx.hash} />
                        </Command.Item>
                      </Command.Group>
                    ))
                  : null}
                {account ? (
                  <Command.Group heading="Accounts">
                    <Command.Item
                      key={account.address}
                      value={account.address}
                      className="group"
                      onSelect={() => [navigate({ to: `/account/${account.address}` }), hideMenu()]}
                    >
                      <SearchItem.Account account={account} />
                    </Command.Item>
                  </Command.Group>
                ) : null}
                {user ? (
                  <Command.Group heading="Users">
                    <Command.Item
                      key={user.name}
                      value={user.name}
                      className="group"
                      onSelect={() => [navigate({ to: `/user/${user.name}` }), hideMenu()]}
                    >
                      <SearchItem.User user={user} />
                    </Command.Item>
                  </Command.Group>
                ) : null}
                {contracts.length ? (
                  <Command.Group heading="Contracts">
                    {contracts.map((contract) => (
                      <Command.Item
                        key={contract.address}
                        value={contract.address}
                        className="group"
                        onSelect={() => [
                          navigate({ to: `/contract/${contract.address}` }),
                          hideMenu(),
                        ]}
                      >
                        <SearchItem.Contract contract={contract} />
                      </Command.Item>
                    ))}
                  </Command.Group>
                ) : null}
              </Command.List>
            </motion.div>
          </div>
        </motion.div>
      )}
    </AnimatePresence>
  );
};

const ExportComponent = Object.assign(SearchMenu, {
  Body,
});

export { ExportComponent as SearchMenu };
