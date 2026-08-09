import type { Address, Client, Message, Signer } from "@left-curve/types";
import { composeTxTypedData } from "@left-curve/utils";
import { getAccountSeenNonces } from "#actions/account-factory/queries/getAccountSeenNonces.js";
import { getAccountSessionSeenNonces } from "#actions/account-factory/queries/getAccountSessionSeenNonces.js";
import { getAccountInfo } from "#actions/account-factory/queries/getAccountInfo.js";
import { type BroadcastTxSyncReturnType, broadcastTxSync } from "./broadcastTxSync.js";

import { queryStatus } from "../queries/queryStatus.js";
import { simulate } from "../queries/simulate.js";

export type SignAndBroadcastTxParameters = {
  sender: Address;
  messages: Message[];
  gasLimit?: number;
};

export type SignAndBroadcastTxReturnType = BroadcastTxSyncReturnType;

export async function signAndBroadcastTx(
  client: Client<Signer>,
  parameters: SignAndBroadcastTxParameters,
): SignAndBroadcastTxReturnType {
  if (!client.signer) throw new Error("client must have a signer");
  const { messages, sender, gasLimit: gas } = parameters;

  const chainId = await (async () => {
    if (client.chain?.id) return client.chain.id;
    const { chainId } = await queryStatus(client);
    return chainId;
  })();

  const [nonce] = client.sessionKey
    ? await getAccountSessionSeenNonces(client, { address: sender, sessionKey: client.sessionKey })
    : await getAccountSeenNonces(client, { address: sender });

  const account = await getAccountInfo(client, { address: sender });

  if (!account) throw new Error("account not found");

  const metadata = {
    chainId,
    userIndex: account.owner,
    nonce,
  };

  const { gasUsed } = gas
    ? { gasUsed: gas }
    : await simulate(client, { simulate: { sender, msgs: messages, data: metadata } });

  const domain = {
    name: "dango",
    chainId: 1,
    verifyingContract: sender,
  };

  const signDoc = composeTxTypedData(
    { messages, gas_limit: gasUsed, data: metadata, sender },
    domain,
  );

  const { credential } = await client.signer.signTx(signDoc);

  const tx = {
    sender,
    credential,
    data: metadata,
    msgs: messages,
    gasLimit: gasUsed,
  };

  return await broadcastTxSync(client, { tx });
}
