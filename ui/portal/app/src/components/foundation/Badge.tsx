import { View, Text } from "react-native";

import { tv } from "tailwind-variants";
import { twMerge } from "@left-curve/foundation";

import type React from "react";
import type { VariantProps } from "tailwind-variants";

export interface BadgeProps extends VariantProps<typeof badgeVariants> {
  text: string;
  classNames?: {
    base?: string;
    text?: string;
  };
}

export const Badge: React.FC<BadgeProps> = ({ text, classNames, ...rest }) => {
  const styles = badgeVariants({ ...rest });
  return (
    <View className={twMerge(styles.base(), classNames?.base)}>
      <Text className={twMerge(styles.text(), classNames?.text)}>{text}</Text>
    </View>
  );
};
const badgeVariants = tv(
  {
    slots: {
      base: "rounded-[4px] diatype-xs-medium w-fit h-fit",
      text: "",
    },
    variants: {
      color: {
        blue: {
          base: "bg-surface-secondary-blue",
          text: "text-fg-primary-blue",
        },
        red: {
          base: "bg-surface-secondary-red border-secondary-red",
          text: "text-fg-primary-red",
        },
        green: {
          base: "bg-surface-tertiary-green border-surface-primary-green",
          text: "text-fg-primary-green",
        },
        rice: {
          base: "bg-surface-quaternary-rice border-outline-secondary-rice",
          text: "text-fg-primary-rice",
        },
      },
      size: {
        s: {
          base: "py-[2px] px-2",
          text: "diatype-xs-medium",
        },
        m: {
          base: "py-[3px] px-1 border",
          text: "diatype-xs-medium",
        },
      },
    },
    defaultVariants: {
      color: "blue",
      size: "m",
    },
  },
  {
    twMerge: true,
  },
);
