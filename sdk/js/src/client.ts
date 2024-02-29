import { Sha256, sha256 } from "@cosmjs/crypto";
import { Comet38Client, type HttpEndpoint } from "@cosmjs/tendermint-rpc";
import type { AbciQueryResponse } from "@cosmjs/tendermint-rpc/build/comet38";
import {
  type AccountResponse,
  type AccountStateResponse,
  type Coin,
  type Config,
  type InfoResponse,
  type Message,
  type Payload,
  type Proof,
  type QueryRequest,
  type QueryResponse,
  type SigningKey,
  deserialize,
  encodeBase64,
  encodeBigEndian32,
  encodeUtf8,
  serialize,
  decodeBase64,
  encodeHex,
  decodeHex,
} from ".";

/**
 * Client for interacting with a CWD blockchain via Tendermint RPC.
 */
export class Client {
  private cometClient: Comet38Client;

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

  private async query(
    path: string,
    data: Uint8Array,
    height = 0,
    prove = false,
  ): Promise<AbciQueryResponse> {
    const res = await this.cometClient.abciQuery({ path, data, height, prove });
    if (res.code !== 0) {
      throw new Error(
        `query failed! codespace: ${res.codespace}, code: ${res.code}, log: ${res.log}`,
      );
    }
    return res;
  }

  public async queryStore(
    key: Uint8Array,
    height = 0,
    prove = false,
  ): Promise<{ value: Uint8Array | null; proof: Proof | null }> {
    const res = await this.query("/store", key, height, prove);
    const value = res.value.length > 0 ? res.value : null;
    let proof = null;
    if (prove) {
      const ops = res.proof!.ops;
      // do some basic sanity checks on the proof op
      if (ops.length !== 1) {
        throw new Error(`expecting exactly one proof op, found ${ops.length}`);
      }
      if (ops[0].type !== "cw_jmt::Proof") {
        throw new Error(`unknown proof type: ${ops[0].type}`);
      }
      if (!arraysIdentical(ops[0].key, key)) {
        throw new Error(
          `incorrect key! expecting: ${encodeBase64(key)}, found: ${encodeBase64(ops[0].key)}`,
        );
      }
      proof = deserialize(ops[0].data) as Proof;
    }
    return { value, proof };
  }

  public async queryApp(req: QueryRequest, height = 0): Promise<QueryResponse> {
    const res = await this.query("/app", serialize(req), height, false);
    return deserialize(res.value) as QueryResponse;
  }

  public async queryInfo(height = 0): Promise<InfoResponse> {
    const res = await this.queryApp(
      {
        info: {},
      },
      height,
    );
    if (!("info" in res)) {
      throw new Error(`expecting info response, got ${JSON.stringify(res)}`);
    }
    return res.info;
  }

  public async queryBalance(address: string, denom: string, height = 0): Promise<number> {
    const res = await this.queryApp(
      {
        balance: { address, denom },
      },
      height,
    );
    if (!("balance" in res)) {
      throw new Error(`expecting balance response, got ${JSON.stringify(res)}`);
    }
    return parseInt(res.balance.amount);
  }

  public async queryBalances(
    address: string,
    startAfter?: string,
    limit?: number,
    height = 0,
  ): Promise<Coin[]> {
    const res = await this.queryApp(
      {
        balances: { address, startAfter, limit },
      },
      height,
    );
    if (!("balances" in res)) {
      throw new Error(`expecting balances response, got ${JSON.stringify(res)}`);
    }
    return res.balances;
  }

  public async querySupply(denom: string, height = 0): Promise<number> {
    const res = await this.queryApp(
      {
        supply: { denom },
      },
      height,
    );
    if (!("supply" in res)) {
      throw new Error(`expecting supply response, got ${JSON.stringify(res)}`);
    }
    return parseInt(res.supply.amount);
  }

  public async querySupplies(startAfter?: string, limit?: number, height = 0): Promise<Coin[]> {
    const res = await this.queryApp(
      {
        supplies: { startAfter, limit },
      },
      height,
    );
    if (!("supplies" in res)) {
      throw new Error(`expecting supplies response, got ${JSON.stringify(res)}`);
    }
    return res.supplies;
  }

  public async queryCode(hash: string, height = 0): Promise<Uint8Array> {
    const res = await this.queryApp(
      {
        code: { hash },
      },
      height,
    );
    if (!("code" in res)) {
      throw new Error(`expecting code response, got ${JSON.stringify(res)}`);
    }
    return decodeBase64(res.code);
  }

  public async queryCodes(startAfter?: string, limit?: number, height = 0): Promise<Uint8Array[]> {
    const res = await this.queryApp(
      {
        codes: { startAfter, limit },
      },
      height,
    );
    if (!("codes" in res)) {
      throw new Error(`expecting codes response, got ${JSON.stringify(res)}`);
    }
    return res.codes.map(decodeHex);
  }

  public async queryAccount(address: string, height = 0): Promise<AccountResponse> {
    const res = await this.queryApp(
      {
        account: { address },
      },
      height,
    );
    if (!("account" in res)) {
      throw new Error(`expecting account response, got ${JSON.stringify(res)}`);
    }
    return res.account;
  }

  public async queryAccounts(
    startAfter?: string,
    limit?: number,
    height = 0,
  ): Promise<AccountResponse[]> {
    const res = await this.queryApp(
      {
        accounts: { startAfter, limit },
      },
      height,
    );
    if (!("accounts" in res)) {
      throw new Error(`expecting accounts response, got ${JSON.stringify(res)}`);
    }
    return res.accounts;
  }

  public async queryWasmRaw(
    contract: string,
    key: Uint8Array,
    height = 0,
  ): Promise<Uint8Array | undefined> {
    const res = await this.queryApp(
      {
        wasmRaw: {
          contract,
          key: encodeBase64(key),
        },
      },
      height,
    );
    if (!("wasmRaw" in res)) {
      throw new Error(`expecting wasm raw response, got ${JSON.stringify(res)}`);
    }
    return res.wasmRaw.value !== undefined ? decodeBase64(res.wasmRaw.value) : undefined;
  }

  public async queryWasmSmart<T>(contract: string, msg: Payload, height = 0): Promise<T> {
    const res = await this.queryApp(
      {
        wasmSmart: {
          contract,
          msg: encodeBase64(serialize(msg)),
        },
      },
      height,
    );
    if (!("wasmSmart" in res)) {
      throw new Error(`expecting wasm smart response, got ${JSON.stringify(res)}`);
    }
    return deserialize(decodeBase64(res.wasmSmart.data)) as T;
  }

  // ------------------------------- tx methods --------------------------------

  public async sendTx(msgs: Message[], signOpts: SigningOptions): Promise<Uint8Array> {
    if (!signOpts.chainId) {
      const infoRes = await this.queryInfo();
      signOpts.chainId = infoRes.chainId;
    }

    if (!signOpts.sequence) {
      const accountStateRes: AccountStateResponse = await this.queryWasmSmart(signOpts.sender, {
        state: {},
      });
      signOpts.sequence = accountStateRes.sequence;
    }

    const tx = serialize(
      await signOpts.signingKey.createAndSignTx(
        msgs,
        signOpts.sender,
        signOpts.chainId,
        signOpts.sequence,
      ),
    );

    const { code, codespace, log, hash } = await this.cometClient.broadcastTxSync({ tx });

    if (code !== 0) {
      throw new Error(
        `failed to broadcast tx! codespace: ${codespace}, code: ${code}, log: ${log}`,
      );
    }

    return hash;
  }

  public async updateConfig(newCfg: Config, signOpts: SigningOptions): Promise<Uint8Array> {
    const updateCfgMsg = {
      updateConfig: { newCfg },
    };
    return this.sendTx([updateCfgMsg], signOpts);
  }

  public async transfer(to: string, coins: Coin[], signOpts: SigningOptions): Promise<Uint8Array> {
    const transferMsg = {
      transfer: { to, coins },
    };
    return this.sendTx([transferMsg], signOpts);
  }

  public async storeCode(wasmByteCode: Uint8Array, signOpts: SigningOptions): Promise<Uint8Array> {
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
    salt: Uint8Array,
    funds: Coin[],
    adminOpt: AdminOption,
    signOpts: SigningOptions,
  ): Promise<[string, Uint8Array]> {
    const address = deriveAddress(signOpts.sender, codeHash, salt);
    const instantiateMsg = {
      instantiate: {
        codeHash: encodeHex(codeHash),
        msg: encodeBase64(serialize(msg)),
        salt: encodeBase64(salt),
        funds,
        admin: createAdmin(adminOpt, signOpts.sender, codeHash, salt),
      },
    };
    const txhash = await this.sendTx([instantiateMsg], signOpts);
    return [address, txhash];
  }

  public async storeCodeAndInstantiate(
    wasmByteCode: Uint8Array,
    msg: Payload,
    salt: Uint8Array,
    funds: Coin[],
    adminOpt: AdminOption,
    signOpts: SigningOptions,
  ): Promise<[string, Uint8Array]> {
    const codeHash = sha256(wasmByteCode);
    const address = deriveAddress(signOpts.sender, codeHash, salt);
    const storeCodeMsg = {
      storeCode: {
        wasmByteCode: encodeBase64(wasmByteCode),
      },
    };
    const instantiateMsg = {
      instantiate: {
        codeHash: encodeHex(codeHash),
        msg: encodeBase64(serialize(msg)),
        salt: encodeBase64(salt),
        funds,
        admin: createAdmin(adminOpt, signOpts.sender, codeHash, salt),
      },
    };
    const txhash = await this.sendTx([storeCodeMsg, instantiateMsg], signOpts);
    return [address, txhash];
  }

  public async execute(
    contract: string,
    msg: Payload,
    funds: Coin[],
    signOpts: SigningOptions,
  ): Promise<Uint8Array> {
    const executeMsg = {
      execute: {
        contract,
        msg: encodeBase64(serialize(msg)),
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
  ): Promise<Uint8Array> {
    const migrateMsg = {
      migrate: {
        contract,
        newCodeHash: encodeHex(newCodeHash),
        msg: encodeBase64(serialize(msg)),
      },
    };
    return this.sendTx([migrateMsg], signOpts);
  }
}

export type SigningOptions = {
  signingKey: SigningKey;
  sender: string;
  chainId?: string;
  sequence?: number;
};

export enum AdminOptionKind {
  SetToSelf = 0,
  SetToNone = 1,
}

export type AdminOption = string | AdminOptionKind.SetToSelf | AdminOptionKind.SetToNone;

/**
 * Determine the admin address based on the given option.
 */
export function createAdmin(
  adminOpt: AdminOption,
  deployer: string,
  codeHash: Uint8Array,
  salt: Uint8Array,
): string | undefined {
  if (adminOpt === AdminOptionKind.SetToSelf) {
    return deriveAddress(deployer, codeHash, salt);
  }
  if (adminOpt === AdminOptionKind.SetToNone) {
    return undefined;
  }
  return adminOpt;
}

/**
 * Derive the salt that is used by the standard account factory contract to
 * register accounts.
 *
 * Mirrors the Rust function: `cw_account_factory::make_salt`.
 */
export function deriveSalt(
  publicKeyType: "secp256k1" | "secp256r1",
  publicKeyBytes: Uint8Array,
  serial: number,
): Uint8Array {
  const hasher = new Sha256();
  hasher.update(encodeUtf8(publicKeyType));
  hasher.update(publicKeyBytes);
  hasher.update(encodeBigEndian32(serial));
  return hasher.digest();
}

/**
 * Derive an account address based on the deployer address, code hash, and salt.
 *
 * Mirrors that Rust function: `cw_std::Addr::compute`
 */
export function deriveAddress(deployer: string, codeHash: Uint8Array, salt: Uint8Array): string {
  const hasher = new Sha256();
  hasher.update(decodeHex(deployer.substring(2))); // strip the 0x prefix
  hasher.update(codeHash);
  hasher.update(salt);
  const bytes = hasher.digest();
  return "0x" + encodeHex(bytes);
}

function arraysIdentical(a: Uint8Array, b: Uint8Array): boolean {
  // check if the two arrays are the same instance
  if (a === b) {
    return true;
  }

  if (a.length !== b.length) {
    return false;
  }

  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) {
      return false;
    }
  }

  return true;
}
