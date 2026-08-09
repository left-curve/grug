export const HYPERLANE_ROUTER_ABI = [
  {
    inputs: [
      { name: "_destination", type: "uint32" },
      { name: "_recipient", type: "bytes32" },
      { name: "_amountOrId", type: "uint256" },
    ],
    name: "transferRemote",
    outputs: [{ name: "", type: "bytes32" }],
    stateMutability: "payable",
    type: "function",
  },
] as const;

export const INFURA_URLS = {
  "1": "https://ethereum-mainnet.core.chainstack.com/70a1cfb855fe87c4abe1dfbcfb58cadb",
  "11155111": "https://ethereum-sepolia-rpc.publicnode.com",
  "42161": "https://arbitrum-mainnet.core.chainstack.com/0e3277a137b9af07fcd4c8088d7f618d",
  "421614": "https://arbitrum-sepolia-rpc.publicnode.com",
};

export const ERC20_ABI = [
  {
    inputs: [
      { name: "spender", type: "address" },
      { name: "amount", type: "uint256" },
    ],
    name: "approve",
    outputs: [{ name: "", type: "bool" }],
    stateMutability: "nonpayable",
    type: "function",
  },
  {
    type: "function",
    name: "balanceOf",
    stateMutability: "view",
    inputs: [{ name: "account", type: "address" }],
    outputs: [{ name: "", type: "uint256" }],
  },
  {
    inputs: [
      { name: "owner", type: "address" },
      { name: "spender", type: "address" },
    ],
    name: "allowance",
    outputs: [{ name: "", type: "uint256" }],
    stateMutability: "view",
    type: "function",
  },
] as const;
