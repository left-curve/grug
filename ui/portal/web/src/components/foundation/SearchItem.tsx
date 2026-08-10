import {
  AddressVisualizer,
  StarToggleButton,
  twMerge,
  useMediaQuery,
} from "@left-curve/applets-kit";
import { useFavApplets } from "@left-curve/store";

import { TruncateText } from "@left-curve/applets-kit";
import { motion } from "framer-motion";
import { Image } from "~/components/foundation/Image";

import type { AccountDetails, Address, ContractInfo, User } from "@left-curve/types";
import type { AnyCoin, AppletMetadata, WithPrice } from "@left-curve/store/types";
import type { PropsWithChildren } from "react";

const childVariants = {
  hidden: { opacity: 0, y: -30 },
  visible: { opacity: 1, y: 0 },
};

const Root: React.FC<PropsWithChildren> = ({ children }) => {
  return <>{children}</>;
};

type SearchAppletItemProps = AppletMetadata;

const AppletItem: React.FC<SearchAppletItemProps> = (applet) => {
  const { id, title, description, img, isDisabled } = applet;
  const { favApplets, addFavApplet, removeFavApplet } = useFavApplets();
  const isFav = favApplets.includes(id);

  return (
    <motion.div
      aria-disabled={isDisabled || undefined}
      className={twMerge(
        "w-full p-2 flex items-center justify-between rounded-xs",
        isDisabled
          ? "cursor-not-allowed text-fg-disabled"
          : "cursor-pointer hover:bg-surface-tertiary-rice group-data-[selected=true]:bg-surface-tertiary-rice",
      )}
      variants={childVariants}
      key={title}
    >
      <div className="flex items-center gap-4">
        <div
          className={twMerge(
            "rounded-xxs border p-1",
            isDisabled
              ? "border-outline-secondary-gray bg-surface-disabled-gray"
              : "border-surface-secondary-red bg-surface-primary-red",
          )}
        >
          <Image
            src={img}
            alt={title}
            className={twMerge("h-12 w-12", isDisabled && "grayscale opacity-50")}
          />
        </div>
        <div>
          <p className={twMerge("diatype-lg-medium", !isDisabled && "text-ink-secondary-700")}>
            {title}
          </p>
          <p className={twMerge("diatype-m-regular", !isDisabled && "text-ink-tertiary-500")}>
            {description}
          </p>
        </div>
      </div>
      <StarToggleButton
        isActive={isFav}
        isDisabled={isDisabled}
        onToggle={() => (isFav ? removeFavApplet(applet) : addFavApplet(applet))}
        className="w-6 h-6 text-primitives-rice-light-500"
      />
    </motion.div>
  );
};

type SearchAssetProps = WithPrice<AnyCoin>;

const AssetItem: React.FC<SearchAssetProps> = ({ logoURI, symbol, price }) => {
  return (
    <motion.div
      className="w-full p-2 min-h-[74px] flex items-start justify-between hover:bg-surface-tertiary-rice rounded-xs group-data-[selected=true]:bg-surface-tertiary-rice cursor-pointer"
      variants={childVariants}
      key={symbol}
    >
      <div className="flex items-start gap-4">
        <Image src={logoURI} alt={symbol} className="w-8 h-8" />
        <div className="flex flex-col gap-1">
          <p className="diatype-m-bold">{symbol}</p>
          <p className="diatype-m-regular text-ink-tertiary-500">{symbol}</p>
          {/* <p className="diatype-m-regular text-ink-tertiary-500">{link}</p> */}
        </div>
      </div>
      <div className="flex flex-col gap-1">
        <p className="diatype-sm-bold">${price}</p>
      </div>
    </motion.div>
  );
};

type SearchBlockItemProps = {
  height: number;
  hash: string;
};

const BlockItem: React.FC<SearchBlockItemProps> = ({ height, hash }) => {
  return (
    <motion.div
      className="w-full p-2 min-h-[74px] flex items-start justify-between hover:bg-surface-tertiary-rice rounded-xs group-data-[selected=true]:bg-surface-tertiary-rice cursor-pointer"
      variants={childVariants}
      key={height}
    >
      <div className="flex items-center gap-4">
        <div className="p-1 bg-surface-primary-red rounded-xxs border border-surface-secondary-red">
          <Image src="/images/emojis/simple/blocks.svg" alt="test" className="w-12 h-12" />
        </div>
        <div className="flex flex-col">
          <p className="diatype-m-medium">#{height} Block</p>
          <TruncateText className="diatype-sm-regular text-ink-tertiary-500" text={hash} end={20} />
        </div>
      </div>
    </motion.div>
  );
};

type SearchTransactionItemProps = {
  height: number;
  hash: string;
};

const TransactionItem: React.FC<SearchTransactionItemProps> = ({ height, hash }) => {
  return (
    <motion.div
      key={hash}
      className="w-full p-2 min-h-[74px] flex items-start justify-between hover:bg-surface-tertiary-rice rounded-xs group-data-[selected=true]:bg-surface-tertiary-rice cursor-pointer"
      variants={childVariants}
    >
      <div className="flex items-center gap-4">
        <div className="p-1 bg-surface-primary-red rounded-xxs border border-surface-secondary-red">
          <Image src="/images/emojis/simple/txs.svg" alt="test" className="w-12 h-12" />
        </div>
        <div className="flex flex-col">
          <TruncateText className="flex gap-2 diatype-m-medium" text={hash} end={20} />

          <p className="diatype-sm-regular text-ink-tertiary-500">Block: #{height}</p>
        </div>
      </div>
    </motion.div>
  );
};

type SearchContractItemProps = {
  contract: ContractInfo & { address: Address };
};

const ContractItem: React.FC<SearchContractItemProps> = ({ contract }) => {
  const { address } = contract;
  const { isMd } = useMediaQuery();
  return (
    <motion.div
      className="w-full p-2 min-h-[74px] flex items-start justify-between hover:bg-surface-tertiary-rice rounded-xs group-data-[selected=true]:bg-surface-tertiary-rice cursor-pointer"
      variants={childVariants}
      key={address}
    >
      <div className="flex items-center gap-4">
        <div className="p-1 bg-surface-primary-red rounded-xxs border border-surface-secondary-red">
          <Image src="/images/emojis/simple/factory.svg" alt="test" className="w-12 h-12" />
        </div>
        <div className="flex flex-col">
          <AddressVisualizer address={address} withIcon classNames={{ text: "diatype-m-medium" }} />
          {isMd ? (
            <p className="diatype-sm-regular text-ink-tertiary-500">{address}</p>
          ) : (
            <TruncateText
              className="diatype-sm-regular text-ink-tertiary-500"
              text={address}
              end={20}
            />
          )}
        </div>
      </div>
    </motion.div>
  );
};

type SearchAccountItemProps = {
  account: AccountDetails;
};

const AccountItem: React.FC<SearchAccountItemProps> = ({ account }) => {
  const { isMd } = useMediaQuery();
  const { username, address } = account;

  const name = `${username ? username : "Account"} #${account?.index}`;

  return (
    <motion.div
      className="w-full p-2 min-h-[74px] flex items-start justify-between hover:bg-surface-tertiary-rice rounded-xs group-data-[selected=true]:bg-surface-tertiary-rice cursor-pointer"
      variants={childVariants}
      key={address}
    >
      <div className="flex items-center gap-4">
        <div className="p-1 bg-surface-primary-red rounded-xxs border border-surface-secondary-red">
          <Image src="/images/emojis/simple/hamster.svg" alt="account" className="w-12 h-12" />
        </div>
        <div className="flex flex-col">
          <p className="flex gap-2 diatype-m-medium">{name}</p>
          {isMd ? (
            <p className="diatype-sm-regular text-ink-tertiary-500">{address}</p>
          ) : (
            <TruncateText
              className="diatype-sm-regular text-ink-tertiary-500"
              text={address}
              end={20}
            />
          )}
        </div>
      </div>
    </motion.div>
  );
};

type SearchUserItemProps = {
  user: User;
};

const UserItem: React.FC<SearchUserItemProps> = ({ user }) => {
  const accountCount = Object.keys(user.accounts).length;

  return (
    <motion.div
      className="w-full p-2 min-h-[74px] flex items-start justify-between hover:bg-surface-tertiary-rice rounded-xs group-data-[selected=true]:bg-surface-tertiary-rice cursor-pointer"
      variants={childVariants}
      key={user.name}
    >
      <div className="flex items-center gap-4">
        <div className="p-1 bg-surface-primary-red rounded-xxs border border-surface-secondary-red">
          <Image src="/images/avatar.png" alt="user" className="w-12 h-12 rounded-lg" />
        </div>
        <div className="flex flex-col">
          <p className="flex gap-2 diatype-m-medium">{user.name}</p>
          <p className="diatype-sm-regular text-ink-tertiary-500">
            {accountCount} {accountCount === 1 ? "account" : "accounts"}
          </p>
        </div>
      </div>
    </motion.div>
  );
};

const ExportComponent = Object.assign(Root, {
  Applet: AppletItem,
  Asset: AssetItem,
  Block: BlockItem,
  Transaction: TransactionItem,
  Account: AccountItem,
  Contract: ContractItem,
  User: UserItem,
});

export { ExportComponent as SearchItem };
