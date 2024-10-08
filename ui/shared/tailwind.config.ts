import { tailwindConfig } from "@leftcurve/config/tailwind/config";
import type { Config } from "tailwindcss";

const config: Config = {
  content: [
    "./.storybook/**/*.{js,ts,jsx,tsx,stories.tsx}",
    "./src/components/**/*.{js,ts,jsx,tsx,stories.tsx}",
    "node_modules/@leftcurve/config/tailwind/**",
  ],
  theme: {
    extend: {},
  },
  presets: [tailwindConfig],
};

export default config;
