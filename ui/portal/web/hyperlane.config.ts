import type { HyperlaneConfig } from "@left-curve/types";

export const hyperlaneConfigs = {
  prod: {
    evm: {
      "1": {
        chainId: 1,
        domain: 1,
        estimatedTime: "6 blocks | 1-3 mins",
        name: "Ethereum Network",
        order: 1,
        protocolFee: 0,
        rpcUrl: "https://ethereum-mainnet.core.chainstack.com/70a1cfb855fe87c4abe1dfbcfb58cadb",
        contracts: {
          mailbox: "0xc005dc82818d67AF737725bD4bf75435d065D239",
          proxyAdmin: "0x613942eff27c6886bb2a33a172cdaf03a009e601",
          staticMessageIdMultisigIsmFactory: "0xfA21D9628ADce86531854C2B7ef00F07394B0B69",
        },
        ism: {
          staticMessageIdMultisigIsm: {
            threshold: 3,
            validators: [
              "0x2F3bC8d740dBfC310D78124db8476040F9Cd7357",
              "0x2cc539a1383128Ab0e2007D24543445A665B1947",
              "0xb45dea8d034AfD3A9753732cAe9eFE15B7f97Fd0",
              "0xCC315d2c581315D5961bdaFd1944B3b4c7DbAC57",
            ],
          },
        },
        routes: [
          {
            type: "erc20Collateral",
            symbol: "USDC",
            tokenAddress: "0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48",
            routerAddress: "0xd05909852ae07118857f9d071781671d12c0f36c",
            implementationAddress: "0xe071653043828c9923c79b04b077358d94fc84f9",
          },
          {
            type: "native",
            symbol: "ETH",
            tokenAddress: "native",
            routerAddress: "0x9d259aa1ec7324c7433b89d2935b08c30f3154cb",
            implementationAddress: "0x9d0ea335355da17ee89e50df43ab823416cf73d4",
          },
        ],
      },
      "42161": {
        chainId: 42161,
        domain: 42161,
        estimatedTime: "1 block | <1 second",
        name: "Arbitrum Network",
        order: 0,
        protocolFee: 0,
        rpcUrl: "https://arbitrum-mainnet.core.chainstack.com/0e3277a137b9af07fcd4c8088d7f618d",
        contracts: {
          mailbox: "0x979ca5202784112f4738403dbec5d0f3b9daabb9",
          proxyAdmin: "0x947303e34c1a2b97fb00c68c1cc4ca97b3361fe6",
          staticMessageIdMultisigIsmFactory: "0x12df53079d399a47e9e730df095b712b0fdfa791",
        },
        ism: {
          staticMessageIdMultisigIsm: {
            threshold: 3,
            validators: [
              "0x2F3bC8d740dBfC310D78124db8476040F9Cd7357",
              "0x2cc539a1383128Ab0e2007D24543445A665B1947",
              "0xb45dea8d034AfD3A9753732cAe9eFE15B7f97Fd0",
              "0xCC315d2c581315D5961bdaFd1944B3b4c7DbAC57",
            ],
          },
        },
        routes: [
          {
            type: "erc20Collateral",
            symbol: "USDC",
            tokenAddress: "0xaf88d065e77c8cc2239327c5edb3a432268e5831",
            routerAddress: "0x9d0ea335355da17ee89e50df43ab823416cf73d4",
            implementationAddress: "0x34dc3f292fc04e3dcc2830ac69bb5d4cd5e8f654",
          },
        ],
      },
    },
  },
  test: {
    evm: {
      "11155111": {
        chainId: 11155111,
        domain: 11155111,
        estimatedTime: "5-30 mins",
        name: "Sepolia Network",
        order: 0,
        protocolFee: 1,
        rpcUrl: "https://ethereum-sepolia-rpc.publicnode.com",
        contracts: {
          mailbox: "0xffaef09b3cd11d9b20d1a19becca54eec2884766",
          proxyAdmin: "0x59cf4f33ce42afa957b93e68031f07bf6d299d60",
          staticMessageIdMultisigIsmFactory: "0xfeb9585b2f948c1ed74034205a7439261a9d27dd",
        },
        ism: {
          staticMessageIdMultisigIsm: {
            threshold: 1,
            validators: [
              "0x6603760598E4aAc3E9D47569cc3A7024cDa7003a",
              "0xf8E81626772C4f6e43e3F4cd9eAac1bC5D23a16f",
              "0x77302BB386b258BA1422bAe9edB5a22EA733D4d4",
            ],
          },
        },
        routes: [
          {
            type: "erc20Collateral",
            symbol: "USDC",
            tokenAddress: "0x1c7d4b196cb0c7b01d743fbc6116a902379c7238",
            routerAddress: "0x0d8c3516df20cff940e479ea2d8c7d1dd0a706ac",
            implementationAddress: "0x26bc0e68467d88cedb5a3793618c8f6586512706",
          },
        ],
      },
      "421614": {
        chainId: 421614,
        domain: 421614,
        estimatedTime: "1 block | <1 second",
        name: "Arbitrum Sepolia Network",
        order: 1,
        protocolFee: 1,
        rpcUrl: "https://arbitrum-sepolia-rpc.publicnode.com",
        contracts: {
          mailbox: "0x598face78a4302f11e3de0bee1894da0b2cb71f8",
          proxyAdmin: "0x947303e34c1a2b97fb00c68c1cc4ca97b3361fe6",
          staticMessageIdMultisigIsmFactory: "0xf7f0dab0bece4498dac7eb616e288809d4499371",
        },
        ism: {
          staticMessageIdMultisigIsm: {
            threshold: 1,
            validators: [
              "0x6603760598E4aAc3E9D47569cc3A7024cDa7003a",
              "0xf8E81626772C4f6e43e3F4cd9eAac1bC5D23a16f",
              "0x77302BB386b258BA1422bAe9edB5a22EA733D4d4",
            ],
          },
        },
        routes: [
          {
            type: "erc20Collateral",
            symbol: "USDC",
            tokenAddress: "0x75faf114eafb1bdbe2f0316df893fd58ce46aa4d",
            routerAddress: "0x9d0ea335355da17ee89e50df43ab823416cf73d4",
            implementationAddress: "0x34dc3f292fc04e3dcc2830ac69bb5d4cd5e8f654",
          },
        ],
      },
    },
  },
} satisfies Record<"prod" | "test", HyperlaneConfig>;

export function getHyperlaneConfig(environment: string): HyperlaneConfig {
  return environment === "prod" ? hyperlaneConfigs.prod : hyperlaneConfigs.test;
}
