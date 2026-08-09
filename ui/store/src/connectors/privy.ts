import { decodeHex, encodeBase64, encodeUtf8, sortedJsonStringify } from "@left-curve/encoding";

import { createKeyHash, createSignerClient, toAccount } from "@left-curve/sdk";
import { getUser } from "@left-curve/sdk/actions";

import { camelToSnake, composeArbitraryTypedData, recursiveTransform } from "@left-curve/utils";
import { createConnector } from "./createConnector.js";

import Privy, {
  getEntropyDetailsFromUser,
  getUserEmbeddedEthereumWallet,
  LocalStorage,
} from "@privy-io/js-sdk-core";

import type { Eip712Signature, JsonValue } from "@left-curve/types";
import type { Address } from "@left-curve/types";
import type { EIP1193Provider } from "../types/eip1193.js";

const ETHEREUM_HEX_CHAIN_ID = "0x1";

type MessagePoster = {
  reload: () => void;
  postMessage: (message: unknown, targetOrigin: string, transfer?: Transferable) => void;
};

type PrivyConnectorParameters = {
  icon?: string;
  appId: string;
  clientId: string;
  poster: (url: string) => MessagePoster;
  listener: (callback: (data: any) => void) => void;
};

export function privy(parameters: PrivyConnectorParameters) {
  const { appId, clientId, poster, listener, icon } = parameters;

  const privy = new Privy({
    appId,
    clientId,
    storage: new LocalStorage(),
  });

  return createConnector<EIP1193Provider>(({ transport, emitter, getUserIndex, chain }) => {
    return {
      id: "privy",
      name: "Privy",
      type: "privy",
      icon,
      privy,
      async setup() {
        privy.setMessagePoster(poster(privy.embeddedWallet.getURL()));
        listener((data) => privy.embeddedWallet.onMessage(data));

        await privy.initialize();
      },
      async connect({ userIndex, chainId, keyHash: _keyHash_ }) {
        const client = createSignerClient({
          signer: this,
          type: "privy",
          chain,
          transport,
        });

        const provider = await this.getProvider();
        await this.switchChain?.({ chainId: ETHEREUM_HEX_CHAIN_ID });

        const user = await getUser(client, { userIndexOrName: { index: userIndex } });
        const accounts = Object.entries(user.accounts).map(([accountIndex, address]) =>
          toAccount({ user, accountIndex: Number(accountIndex), address: address as Address }),
        );

        const keyHash = await (async () => {
          if (_keyHash_) return _keyHash_;
          const [controllerAddress] = await provider.request({ method: "eth_requestAccounts" });

          return createKeyHash(controllerAddress.toLowerCase());
        })();

        if (!user.keys[keyHash]) throw new Error("Not authorized");

        const account = accounts[0];
        const userStatus = await client.getAccountStatus({ address: account.address });

        emitter.emit("connect", {
          accounts,
          chainId,
          userIndex,
          keyHash,
          userStatus,
          username: user.name,
        });
      },
      async disconnect() {
        emitter.emit("disconnect");
      },
      async getClient() {
        return createSignerClient({
          signer: this,
          type: "privy",
          chain,
          transport,
        });
      },
      async getKeyHash() {
        const provider = await this.getProvider();
        const [controllerAddress] = await provider.request({ method: "eth_requestAccounts" });
        const addressLowerCase = controllerAddress.toLowerCase();
        return createKeyHash(addressLowerCase);
      },
      async getProvider() {
        const { user } = await privy.user.get();
        if (!user) throw new Error("we couldn't recover the session");
        const wallet = getUserEmbeddedEthereumWallet(user)!;
        const { entropyId, entropyIdVerifier } = getEntropyDetailsFromUser(user)!;

        return (await privy.embeddedWallet.getEthereumProvider({
          wallet,
          entropyId,
          entropyIdVerifier,
        })) as unknown as EIP1193Provider;
      },
      async getAccounts() {
        const client = await this.getClient();
        const userIndex = getUserIndex();
        if (userIndex === undefined) throw new Error("privy: user index not found");

        const user = await getUser(client, { userIndexOrName: { index: userIndex } });
        return Object.entries(user.accounts).map(([accountIndex, address]) =>
          toAccount({ user, accountIndex: Number(accountIndex), address: address as Address }),
        );
      },
      async switchChain({ chainId }) {
        const provider = await this.getProvider();

        await provider.request({
          method: "wallet_switchEthereumChain",
          params: [{ chainId }],
        });
      },
      async isAuthorized() {
        const provider = await this.getProvider();
        await this.switchChain?.({ chainId: ETHEREUM_HEX_CHAIN_ID });
        const [controllerAddress] = await provider.request({ method: "eth_accounts" });
        const accounts = await this.getAccounts();
        return !!controllerAddress && accounts.length > 0;
      },
      async signArbitrary(payload) {
        const { types, primaryType, message } = payload;

        const provider = await this.getProvider();
        await this.switchChain?.({ chainId: ETHEREUM_HEX_CHAIN_ID });
        const [controllerAddress] = await provider.request({ method: "eth_requestAccounts" });

        // EIP-712 has no sum type, so any field the doc declares as `string`
        // but whose value is an object (e.g. onboarding's `key`, a `Key` enum)
        // is bound as its canonical JSON string -- matching the chain's
        // reconstruction. The non-EIP-712 signers sign the object form instead.
        const boundMessage = { ...(message as Record<string, JsonValue>) };
        for (const { name, type } of types[primaryType] ?? []) {
          const value = boundMessage[name];
          if (type === "string" && typeof value === "object" && value !== null) {
            boundMessage[name] = sortedJsonStringify(recursiveTransform(value, camelToSnake));
          }
        }

        const typedData = composeArbitraryTypedData({ message: boundMessage, types, primaryType });
        const signData = JSON.stringify(typedData);

        const signature = await provider.request({
          method: "eth_signTypedData_v4",
          params: [controllerAddress, signData],
        });

        const eip712: Eip712Signature = {
          sig: encodeBase64(decodeHex(signature.slice(2))),
          typed_data: encodeBase64(encodeUtf8(signData)),
        };

        const keyHash = createKeyHash(controllerAddress.toLowerCase());

        return {
          credential: { standard: { keyHash, signature: { eip712 } } },
          signed: payload,
        };
      },
      async signTx(signDoc) {
        const provider = await this.getProvider();
        await this.switchChain?.({ chainId: ETHEREUM_HEX_CHAIN_ID });
        const [controllerAddress] = await provider.request({ method: "eth_requestAccounts" });

        // EIP-712 has no sum type, so bind each message as its canonical JSON
        // string (recursively key-sorted, compact) -- matching the `string[]`
        // type declared in the doc and the chain's reconstruction. The rest of
        // the doc (shared with the non-EIP-712 signers) keeps message objects.
        const eip712SignDoc = {
          ...signDoc,
          message: {
            ...signDoc.message,
            messages: signDoc.message.messages.map((msg) => sortedJsonStringify(msg)),
          },
        };

        const signData = JSON.stringify(eip712SignDoc);

        const signature = await provider.request({
          method: "eth_signTypedData_v4",
          params: [controllerAddress, signData],
        });

        const eip712: Eip712Signature = {
          sig: encodeBase64(decodeHex(signature.slice(2))),
          typed_data: encodeBase64(encodeUtf8(signData)),
        };

        const keyHash = createKeyHash(controllerAddress.toLowerCase());

        const standard = { signature: { eip712 }, keyHash };

        return { credential: { standard }, signed: signDoc };
      },
    };
  });
}
