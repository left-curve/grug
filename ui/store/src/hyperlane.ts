import { sepolia, mainnet, arbitrum, arbitrumSepolia } from "viem/chains";

export const chains = {
  "11155111": {
    ...sepolia,
    contracts: {
      ...sepolia.contracts,
      erc20: [
        {
          symbol: "USDC",
          address: "0x1c7d4b196cb0c7b01d743fbc6116a902379c7238",
          decimals: 6,
          targetDenom: "bridge/usdc",
        },
      ],
    },
  },
  "1": {
    ...mainnet,
    contracts: {
      ...mainnet.contracts,
      erc20: [
        {
          symbol: "USDT",
          address: "0xdAC17F958D2ee523a2206206994597C13D831ec7",
          decimals: 6,
          targetDenom: "bridge/usdt",
        },
        {
          symbol: "USDC",
          address: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
          decimals: 6,
          targetDenom: "bridge/usdc",
        },
      ],
    },
  },
  "42161": {
    ...arbitrum,
    contracts: {
      ...arbitrum.contracts,
      erc20: [
        {
          symbol: "USDT",
          address: "0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9",
          decimals: 6,
          targetDenom: "bridge/usdt",
        },
        {
          symbol: "USDC",
          address: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
          decimals: 6,
          targetDenom: "bridge/usdc",
        },
      ],
    },
  },
  "421614": {
    ...arbitrumSepolia,
    contracts: {
      ...arbitrumSepolia.contracts,
      erc20: [
        {
          symbol: "USDC",
          address: "0x75faf114eafb1BDbe2F0316DF893fd58CE46AA4d",
          decimals: 6,
          targetDenom: "bridge/usdc",
        },
      ],
    },
  },
};
