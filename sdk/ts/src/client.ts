import { Secp256k1, Sha256, sha256 } from "@cosmjs/crypto";
import { Comet38Client, HttpEndpoint } from "@cosmjs/tendermint-rpc";
import { BroadcastTxSyncResponse } from "@cosmjs/tendermint-rpc/build/comet38";
import {
  decodeUtf8,
  deserialize,
  encodeBase64,
  encodeBigEndian32,
  encodeHex,
  encodeUtf8,
  serialize,
  Payload,
} from "./serde";
import {
  Account,
  AccountResponse,
  AccountStateResponse,
  Coin,
  Config,
  InfoResponse,
  Message,
  QueryRequest,
  QueryResponse,
} from "./types";

/**
 * Client for interacting with a CWD blockchain via Tendermint RPC.
 */
export class Client {
  cometClient: Comet38Client;

  /**
   * Do not use; use `Client.connect` instead.
   */
  private constructor(cometClient: Comet38Client) {
    this.cometClient = cometClient;
  }

  /**
   * Create a new CWD client for the given endpoint.
   *
   * Uses HTTP when the URL schema is http or https. Uses WebSockets otherwise.
   */
  public static async connect(endpoint: string | HttpEndpoint): Promise<Client> {
    const cometClient = await Comet38Client.connect(endpoint);
    return new Client(cometClient);
  }

  // ------------------------------ query methods ------------------------------

  private async query(req: QueryRequest): Promise<QueryResponse> {
    const res = await this.cometClient.abciQuery({
      path: "app",
      data: encodeUtf8(serialize(req)),
    });

    if (res.code !== 0) {
      throw new Error(`query failed! codespace: ${res.codespace}, code: ${res.code}, log: ${res.log}`);
    }

    return deserialize(decodeUtf8(res.value)) as QueryResponse;
  }

  public async queryInfo(): Promise<InfoResponse> {
    const res = await this.query({ info: {} });
    return res.info!;
  }

  public async queryBalance(address: string, denom: string): Promise<string> {
    const res = await this.query({ balance: { address, denom } });
    return res.balance!.amount;
  }

  public async queryBalances(address: string, startAfter?: string, limit?: number): Promise<Coin[]> {
    const res = await this.query({ balances: { address, startAfter, limit } });
    return res.balances!;
  }

  public async querySupply(denom: string): Promise<string> {
    const res = await this.query({ supply: { denom } });
    return res.supply!.amount;
  }

  public async querySupplies(startAfter?: string, limit?: number): Promise<Coin[]> {
    const res = await this.query({ supplies: { startAfter, limit } });
    return res.supplies!;
  }

  public async queryCode(hash: string): Promise<string> {
    const res = await this.query({ code: { hash } });
    return res.code!;
  }

  public async queryCodes(startAfter?: string, limit?: number): Promise<string[]> {
    const res = await this.query({ codes: { startAfter, limit } });
    return res.codes!;
  }

  public async queryAccount(address: string): Promise<Account> {
    const res = await this.query({ account: { address } });
    const accountRes = res.account!;
    return {
      codeHash: accountRes.codeHash,
      admin: accountRes.admin,
    }
  }

  public async queryAccounts(startAfter?: string, limit?: number): Promise<AccountResponse[]> {
    const res = await this.query({ accounts: { startAfter, limit } });
    return res.accounts!;
  }

  public async queryWasmRaw(contract: string, key: string): Promise<string | undefined> {
    const res = await this.query({ wasmRaw: { contract, key } });
    return res.wasmRaw!.value;
  }

  public async queryWasmSmart<T>(contract: string, msg: Payload): Promise<T> {
    const res = await this.query({ wasmSmart: { contract, msg: btoa(serialize(msg)) } });
    const wasmRes = deserialize(atob(res.wasmSmart!.data));
    return wasmRes as T;
  }

  // ------------------------------- tx methods --------------------------------

  public async sendTx(msgs: Message[], signOpts: SigningOptions): Promise<BroadcastTxSyncResponse> {
    if (!signOpts.chainId) {
      const infoRes = await this.queryInfo();
      signOpts.chainId = infoRes.chainId;
    }

    if (!signOpts.sequence) {
      const accountStateRes: AccountStateResponse = await this.queryWasmSmart(signOpts.sender, { state: {} });
      signOpts.sequence = accountStateRes.sequence;
    }

    const signBytes = createSignBytes(msgs, signOpts.sender, signOpts.chainId, signOpts.sequence);
    const signature = await Secp256k1.createSignature(signBytes, signOpts.signingKey);

    const tx = encodeUtf8(serialize({
      sender: signOpts.sender,
      msgs,
      credential: encodeBase64(signature.toDer()),
    }));

    return this.cometClient.broadcastTxSync({ tx });
  }

  public async updateConfig(
    newCfg: Config,
    signOpts: SigningOptions,
  ): Promise<BroadcastTxSyncResponse> {
    const updateCfgMsg = {
      updateConfig: { newCfg },
    };
    return this.sendTx([updateCfgMsg], signOpts);
  }

  public async transfer(
    to: string,
    coins: Coin[],
    signOpts: SigningOptions,
  ): Promise<BroadcastTxSyncResponse> {
    const transferMsg = {
      transfer: { to, coins },
    };
    return this.sendTx([transferMsg], signOpts);
  }

  public async storeCode(
    wasmByteCode: Uint8Array,
    signOpts: SigningOptions,
  ): Promise<BroadcastTxSyncResponse> {
    const storeCodeMsg = {
      storeCode: {
        wasmByteCode: encodeBase64(wasmByteCode),
      },
    };
    return this.sendTx([storeCodeMsg], signOpts);
  }

  public async instantiate(
    codeHash: Uint8Array,
    msg: Payload,
    salt: string,
    funds: Coin[],
    admin: string | undefined,
    signOpts: SigningOptions,
  ): Promise<BroadcastTxSyncResponse> {
    const instantiateMsg = {
      instantiate: {
        codeHash: encodeHex(codeHash),
        msg: btoa(serialize(msg)),
        salt,
        funds,
        admin,
      },
    };
    return this.sendTx([instantiateMsg], signOpts);
  }

  public async storeCodeAndInstantiate(
    wasmByteCode: Uint8Array,
    msg: Payload,
    salt: string,
    funds: Coin[],
    admin: string | undefined,
    signOpts: SigningOptions,
  ): Promise<BroadcastTxSyncResponse> {
    const storeCodeMsg = {
      storeCode: {
        wasmByteCode: encodeBase64(wasmByteCode),
      },
    };
    const instantiateMsg = {
      instantiate: {
        codeHash: encodeHex(sha256(wasmByteCode)),
        msg: btoa(serialize(msg)),
        salt,
        funds,
        admin,
      },
    };
    return this.sendTx([storeCodeMsg, instantiateMsg], signOpts);
  }

  public async execute(
    contract: string,
    msg: Payload,
    funds: Coin[],
    signOpts: SigningOptions,
  ): Promise<BroadcastTxSyncResponse> {
    const executeMsg = {
      execute: {
        contract,
        msg: btoa(serialize(msg)),
        funds,
      },
    };
    return this.sendTx([executeMsg], signOpts);
  }

  public async migrate(
    contract: string,
    newCodeHash: Uint8Array,
    msg: Payload,
    signOpts: SigningOptions,
  ): Promise<BroadcastTxSyncResponse> {
    const migrateMsg = {
      migrate: {
        contract,
        newCodeHash: encodeHex(newCodeHash),
        msg: btoa(serialize(msg)),
      },
    };
    return this.sendTx([migrateMsg], signOpts);
  }
}

export type SigningOptions = {
  sender: string;
  signingKey: Uint8Array;
  chainId?: string;
  sequence?: number;
};

/**
 * Generate sign byte that the cw-account contract expects.
 */
export function createSignBytes(
  msgs: Message[],
  sender: string,
  chainId: string,
  sequence: number,
): Uint8Array {
  let hasher = new Sha256();
  hasher.update(encodeUtf8(serialize(msgs)));
  hasher.update(encodeUtf8(sender));
  hasher.update(encodeUtf8(chainId));
  hasher.update(encodeBigEndian32(sequence));
  return hasher.digest();
}
