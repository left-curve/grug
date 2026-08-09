import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, resolve } from "node:path";
import { describe, expect, it } from "vitest";

const projectRoot = process.cwd();
const selfPath = "tests/test-quality-guards.test.tsx";
const scannedRoots = ["src", "tests"].map((path) => resolve(projectRoot, path));
const testRoot = resolve(projectRoot, "tests");
const englishMessagesPath = resolve(projectRoot, "../../foundation/messages/en.json");
const userIndexTruthyScanRoots = ["src", "../../store/src"].map((path) =>
  resolve(projectRoot, path),
);
const sourceExtensions = new Set([".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"]);
const bech32DangoAddressPattern = new RegExp(["dango", "1"].join(""), "i");
const appletsKitMockHelperPath = "tests/mocks/applets-kit.ts";
const sharedAppletsKitMockOverrides = new Set([
  "Marquee",
  "QRCodeReader",
  "TextLoop",
  "useApp",
  "useAnimateOnce",
  "useBodyScrollLock",
  "useClickAway",
  "useCountdown",
  "useHeaderHeight",
  "useInfiniteScroll",
  "useMediaQuery",
  "usePortalTarget",
  "usePreserveScroll",
  "useTheme",
]);
const disallowedAppletsKitMockOverrides = new Set(["Button"]);
// Local applets-kit mocks are reserved for component/function shims that the shared helper cannot
// express. Tests that only control shared boundaries should import tests/mocks/applets-kit.ts instead.
const reviewedLocalAppletsKitMockFiles = new Set([
  "tests/bridge-ui.test.tsx",
  "tests/mocks/transfer.tsx",
]);
const reviewedLocalAppletsKitMockOverrides = [
  "tests/bridge-ui.test.tsx:AssetInputWithRange,CoinSelector,ConnectWalletWithModal,FormattedNumber,IconDisconnect,Input,Modals,NetworkSelector,useApp,useTheme",
  "tests/mocks/transfer.tsx:AccountSearchInput,CoinSelector,Tab,Tabs,useApp",
].sort();
const reviewedExplicitSkips = [
  "tests/pages/referral.spec.ts:User is not a referrer - Edit icon not visible (no trading volume)",
  "tests/pages/transfer.spec.ts:send transaction flow",
];
const reviewedHardcodedMessageLiteralCandidates = [
  "tests/message-exchanger.test.tsx:WebSocket:1",
  "tests/passkey-connector.test.tsx:Transfer:1",
  "tests/points-leaderboard-table.test.tsx:No data available:1",
  "tests/pro-trade-history.test.tsx:Ethereum:1",
  "tests/search-token.test.tsx:Ethereum:1",
  "tests/trading-view.test.tsx:TradingView:1",
  "tests/use-search-bar.test.tsx:Transfer:1",
].sort();
const reviewedUserIndexTruthinessChecks = [
  "../../store/src/hooks/useBoosters.ts:enabled: enabled && !!userIndex,",
  "../../store/src/hooks/useBoxes.ts:enabled: enabled && !!userIndex,",
  "../../store/src/hooks/useEpochPoints.ts:enabled: enabled && !!userIndex,",
  "../../store/src/hooks/usePoints.ts:enabled: enabled && !!userIndex,",
  "../../store/src/hooks/useReferral.ts:enabled: enabled && !!userIndex && !!client && !!perpsAddress,",
  "../../store/src/hooks/useReferral.ts:enabled: enabled && !!userIndex && !!client && !!perpsAddress,",
  "../../store/src/hooks/useReferral.ts:enabled: enabled && !!userIndex && !!client && !!perpsAddress,",
  "../../store/src/hooks/useReferral.ts:enabled: enabled && !!userIndex && !!client,",
  '../../store/src/hooks/useReferral.ts:if (!userIndex) return "";',
  '../../store/src/hooks/useReferral.ts:if (!userIndex) return "";',
  "../../store/src/actions/reconnect.ts:const user = userIndex ?",
  "src/components/modals/AddKey.tsx:enabled: !!signingClient && !!userIndex,",
  "src/components/points/PointsHeader.tsx:const { predictedPoints } = usePredictPoints({ pointsUrl, userIndex, enabled: isStarted && !!userIndex });",
  "src/components/points/leaderboard/LeaderboardTable.tsx:if (userIndex) {",
  "src/components/points/referral/MyCommission.tsx:enabled: !!client && !!userIndex,",
  "src/components/points/rewards/useChestOpening.tsx:if (hasSpun && userIndex && currentVariant && slotSequence.length > 0) {",
  "src/components/settings/KeyManagementSection.tsx:enabled: !!signingClient && !!userIndex,",
].sort();

function* sourceFiles(dir: string): Generator<string> {
  for (const entry of readdirSync(dir)) {
    const path = join(dir, entry);
    const stats = statSync(path);

    if (stats.isDirectory()) {
      if (entry === "node_modules" || entry === "coverage" || entry === "dist") continue;
      yield* sourceFiles(path);
      continue;
    }

    const extension = entry.slice(entry.lastIndexOf("."));
    if (!sourceExtensions.has(extension)) continue;

    const relativePath = relative(projectRoot, path);
    if (relativePath === selfPath) continue;
    yield path;
  }
}

function findViolations(pattern: RegExp) {
  return scannedRoots.flatMap((root) =>
    Array.from(sourceFiles(root)).flatMap((path) => {
      const content = readFileSync(path, "utf8");
      return pattern.test(content) ? [relative(projectRoot, path)] : [];
    }),
  );
}

function lineAt(content: string, index: number) {
  return content.slice(0, index).split("\n").length;
}

function mockFactorySnippet(content: string, startIndex: number) {
  const nextMockIndex = content.indexOf("\nvi.mock(", startIndex + 1);
  const nextDescribeIndex = content.indexOf("\ndescribe(", startIndex + 1);
  const nextIndexes = [nextMockIndex, nextDescribeIndex].filter((index) => index !== -1);
  const endIndex = nextIndexes.length ? Math.min(...nextIndexes) : content.length;
  return content.slice(startIndex, endIndex);
}

function skipQuotedContent(content: string, index: number, quote: string) {
  let i = index + 1;
  while (i < content.length) {
    if (content[i] === "\\") {
      i += 2;
      continue;
    }
    if (content[i] === quote) return i;
    i += 1;
  }
  return content.length - 1;
}

function skipLineComment(content: string, index: number) {
  const endIndex = content.indexOf("\n", index + 2);
  return endIndex === -1 ? content.length - 1 : endIndex;
}

function skipBlockComment(content: string, index: number) {
  const endIndex = content.indexOf("*/", index + 2);
  return endIndex === -1 ? content.length - 1 : endIndex + 1;
}

function splitTopLevelObjectProperties(content: string, objectStartIndex: number) {
  const properties: string[] = [];
  let depth = 0;
  let propertyStart = objectStartIndex + 1;

  for (let index = objectStartIndex; index < content.length; index += 1) {
    const char = content[index];
    const nextChar = content[index + 1];

    if (char === '"' || char === "'" || char === "`") {
      index = skipQuotedContent(content, index, char);
      continue;
    }

    if (char === "/" && nextChar === "/") {
      index = skipLineComment(content, index);
      continue;
    }

    if (char === "/" && nextChar === "*") {
      index = skipBlockComment(content, index);
      continue;
    }

    if (char === "{" || char === "(" || char === "[") {
      depth += 1;
      continue;
    }

    if (char === "}" || char === ")" || char === "]") {
      depth -= 1;
      if (depth === 0) {
        properties.push(content.slice(propertyStart, index));
        break;
      }
      continue;
    }

    if (char === "," && depth === 1) {
      properties.push(content.slice(propertyStart, index));
      propertyStart = index + 1;
    }
  }

  return properties;
}

function topLevelObjectKeys(content: string, objectStartIndex: number) {
  return splitTopLevelObjectProperties(content, objectStartIndex).flatMap((property) => {
    const trimmedProperty = property.trim();
    if (!trimmedProperty || trimmedProperty.startsWith("...")) return [];
    const quotedKeyMatch = /^["']([^"']+)["']\s*:/.exec(trimmedProperty);
    if (quotedKeyMatch) return [quotedKeyMatch[1]];
    const identifierKeyMatch = /^([A-Za-z_$][\w$]*)\s*:/.exec(trimmedProperty);
    if (identifierKeyMatch) return [identifierKeyMatch[1]];
    return [];
  });
}

function isActualModuleSpread(property: string) {
  const trimmedProperty = property.trim();
  return /^\.\.\.\s*(?:actual|\(\s*await\s+importOriginal|\(\s*await\s+importOriginal<)/.test(
    trimmedProperty,
  );
}

function findReturnedActualModuleObject(content: string, mockStartIndex: number) {
  const snippet = mockFactorySnippet(content, mockStartIndex);
  const snippetStart = mockStartIndex;

  for (let index = 0; index < snippet.length; index += 1) {
    const char = snippet[index];
    const nextChar = snippet[index + 1];

    if (char === '"' || char === "'" || char === "`") {
      index = skipQuotedContent(snippet, index, char);
      continue;
    }

    if (char === "/" && nextChar === "/") {
      index = skipLineComment(snippet, index);
      continue;
    }

    if (char === "/" && nextChar === "*") {
      index = skipBlockComment(snippet, index);
      continue;
    }

    if (char !== "{") continue;

    const absoluteIndex = snippetStart + index;
    const properties = splitTopLevelObjectProperties(content, absoluteIndex);
    if (properties.some(isActualModuleSpread)) return absoluteIndex;
  }

  return -1;
}

function appletsKitMockOverrideKeys(content: string, mockStartIndex: number) {
  const objectStartIndex = findReturnedActualModuleObject(content, mockStartIndex);
  if (objectStartIndex === -1) return [];
  return topLevelObjectKeys(content, objectStartIndex);
}

function findAppletsKitMockViolations() {
  const mockPattern = /vi\.mock\(\s*["']@left-curve\/applets-kit["']\s*,/g;
  const importOriginalCallPattern = /\bawait\s+importOriginal\b|\bimportOriginal(?:<|\s*\()/;

  return scannedRoots.flatMap((root) =>
    Array.from(sourceFiles(root)).flatMap((path) => {
      const content = readFileSync(path, "utf8");
      const relativePath = relative(projectRoot, path);
      const violations: string[] = [];

      for (const match of content.matchAll(mockPattern)) {
        const snippet = content.slice(match.index, match.index + 800);
        if (!importOriginalCallPattern.test(snippet)) {
          violations.push(`${relativePath}:${lineAt(content, match.index)}`);
        }
      }

      return violations;
    }),
  );
}

function findCommonHookOnlyLocalAppletsKitMocks() {
  const mockPattern = /vi\.mock\(\s*["']@left-curve\/applets-kit["']\s*,/g;

  return scannedRoots.flatMap((root) =>
    Array.from(sourceFiles(root)).flatMap((path) => {
      const content = readFileSync(path, "utf8");
      const relativePath = relative(projectRoot, path);
      if (relativePath === appletsKitMockHelperPath) return [];

      const violations: string[] = [];
      for (const match of content.matchAll(mockPattern)) {
        const overrideKeys = appletsKitMockOverrideKeys(content, match.index);
        const specificOverrideKeys = overrideKeys.filter(
          (key) => !sharedAppletsKitMockOverrides.has(key),
        );
        if (specificOverrideKeys.length === 0) {
          violations.push(`${relativePath}:${lineAt(content, match.index)}`);
        }
      }

      return violations;
    }),
  );
}

function findUnreviewedLocalAppletsKitMocks() {
  const mockPattern = /vi\.mock\(\s*["']@left-curve\/applets-kit["']\s*,/;

  return scannedRoots.flatMap((root) =>
    Array.from(sourceFiles(root)).flatMap((path) => {
      const content = readFileSync(path, "utf8");
      const relativePath = relative(projectRoot, path);
      if (!mockPattern.test(content)) return [];
      if (relativePath === appletsKitMockHelperPath) return [];
      if (reviewedLocalAppletsKitMockFiles.has(relativePath)) return [];
      return [relativePath];
    }),
  );
}

function findLocalAppletsKitMockOverrides() {
  const mockPattern = /vi\.mock\(\s*["']@left-curve\/applets-kit["']\s*,/g;

  return scannedRoots
    .flatMap((root) =>
      Array.from(sourceFiles(root)).flatMap((path) => {
        const content = readFileSync(path, "utf8");
        const relativePath = relative(projectRoot, path);
        if (relativePath === appletsKitMockHelperPath) return [];

        return Array.from(content.matchAll(mockPattern)).map((match) => {
          const overrideKeys = appletsKitMockOverrideKeys(content, match.index);
          return `${relativePath}:${overrideKeys.join(",")}`;
        });
      }),
    )
    .sort();
}

function findSharedAppletsKitMockOverrides() {
  const mockPattern = /vi\.mock\(\s*["']@left-curve\/applets-kit["']\s*,/;
  const content = readFileSync(resolve(projectRoot, appletsKitMockHelperPath), "utf8");
  const match = mockPattern.exec(content);
  if (match?.index === undefined) return [];
  return appletsKitMockOverrideKeys(content, match.index).sort();
}

function findDisallowedAppletsKitMockOverrides() {
  const mockPattern = /vi\.mock\(\s*["']@left-curve\/applets-kit["']\s*,/g;

  return scannedRoots
    .flatMap((root) =>
      Array.from(sourceFiles(root)).flatMap((path) => {
        const content = readFileSync(path, "utf8");
        const relativePath = relative(projectRoot, path);

        return Array.from(content.matchAll(mockPattern)).flatMap((match) => {
          const overrideKeys = appletsKitMockOverrideKeys(content, match.index);
          return overrideKeys
            .filter((key) => disallowedAppletsKitMockOverrides.has(key))
            .map((key) => `${relativePath}:${lineAt(content, match.index)}:${key}`);
        });
      }),
    )
    .sort();
}

function findExplicitSkips() {
  const skipPattern = /\b(?:test|it|describe)\.skip\s*\(/g;
  const titlePattern = /["'`]([^"'`]+)["'`]/;

  return scannedRoots
    .flatMap((root) =>
      Array.from(sourceFiles(root)).flatMap((path) => {
        const content = readFileSync(path, "utf8");
        const relativePath = relative(projectRoot, path);

        return Array.from(content.matchAll(skipPattern)).map((match) => {
          const snippet = content.slice(match.index, match.index + 500);
          const title = titlePattern.exec(snippet)?.[1] ?? `line ${lineAt(content, match.index)}`;
          return `${relativePath}:${title}`;
        });
      }),
    )
    .sort();
}

function findFakeTimerCleanupViolations() {
  const fakeTimersPattern = /\bvi\.useFakeTimers\s*\(/;
  const realTimersPattern = /\bvi\.useRealTimers\s*\(/;

  return scannedRoots
    .flatMap((root) =>
      Array.from(sourceFiles(root)).flatMap((path) => {
        const content = readFileSync(path, "utf8");
        if (!fakeTimersPattern.test(content)) return [];
        if (realTimersPattern.test(content)) return [];
        return [relative(projectRoot, path)];
      }),
    )
    .sort();
}

function collectEnglishMessageLiteralValues(
  value: unknown,
  path: string[] = [],
  literals = new Set<string>(),
) {
  if (Array.isArray(value)) {
    value.forEach((entry, index) => {
      collectEnglishMessageLiteralValues(entry, [...path, String(index)], literals);
    });
    return literals;
  }

  if (value && typeof value === "object") {
    Object.entries(value).forEach(([key, entry]) => {
      if (key !== "$schema") collectEnglishMessageLiteralValues(entry, [...path, key], literals);
    });
    return literals;
  }

  if (typeof value !== "string") return literals;

  const key = path.join(".");
  if (value.length < 8) return literals;
  if (/[{}\n\t<]/.test(value)) return literals;
  if (/(^|\.)(id|path)$/.test(key)) return literals;
  if (key.includes(".placeholder.")) return literals;
  if (key.includes(".selectors.") || key.includes(".declarations.")) return literals;
  if (key === "transfer.warning.withdraw") return literals;

  literals.add(value);
  return literals;
}

function countQuotedLiteral(content: string, value: string) {
  return [`"${value}"`, `'${value}'`, `\`${value}\``].reduce((count, quotedValue) => {
    let index = content.indexOf(quotedValue);
    let nextCount = count;

    while (index !== -1) {
      nextCount += 1;
      index = content.indexOf(quotedValue, index + quotedValue.length);
    }

    return nextCount;
  }, 0);
}

function findHardcodedMessageLiteralCandidates() {
  const messageLiterals = collectEnglishMessageLiteralValues(
    JSON.parse(readFileSync(englishMessagesPath, "utf8")),
  );

  return Array.from(sourceFiles(testRoot))
    .flatMap((path) => {
      const content = readFileSync(path, "utf8");
      const relativePath = relative(projectRoot, path);
      return Array.from(messageLiterals).flatMap((literal) => {
        const count = countQuotedLiteral(content, literal);
        return count ? [`${relativePath}:${literal}:${count}`] : [];
      });
    })
    .sort();
}

function findUserIndexTruthinessChecks() {
  const truthinessPattern =
    /!!userIndex|Boolean\(\s*userIndex\s*\)|\buserIndex\s*&&|\buserIndex\s*\?(?!:)|\bif\s*\(\s*!userIndex\s*\)|\bif\s*\(\s*userIndex\s*\)/;
  const multilineTernaryStartPattern = /\buserIndex\s*$/;
  const multilineTernaryContinuationPattern = /^\s*\?/;

  return userIndexTruthyScanRoots
    .flatMap((root) =>
      Array.from(sourceFiles(root)).flatMap((path) => {
        const relativePath = relative(projectRoot, path);
        const lines = readFileSync(path, "utf8").split("\n");

        return lines.flatMap((line, index) => {
          const violations = truthinessPattern.test(line) ? [`${relativePath}:${line.trim()}`] : [];
          const nextLine = lines[index + 1] ?? "";
          if (
            multilineTernaryStartPattern.test(line) &&
            multilineTernaryContinuationPattern.test(nextLine)
          ) {
            violations.push(`${relativePath}:${line.trim()} ?`);
          }

          return violations;
        });
      }),
    )
    .sort();
}

describe("portal test quality guards", () => {
  it("uses real Paraglide messages instead of hardcoded message mocks", () => {
    expect(findViolations(/vi\.mock\(\s*["'][^"']*paraglide\/messages(?:\.js)?["']/)).toEqual([]);
  });

  it("uses 0x Dango addresses instead of bech32-looking Dango fixtures", () => {
    expect(findViolations(bech32DangoAddressPattern)).toEqual([]);
  });

  it("keeps applets-kit mocks partial by importing the real module first", () => {
    expect(findAppletsKitMockViolations()).toEqual([]);
  });

  it("keeps shared applets-kit boundaries in the shared helper", () => {
    expect(findUnreviewedLocalAppletsKitMocks()).toEqual([]);
  });

  it("keeps local applets-kit mocks tied to component or function shims", () => {
    expect(findCommonHookOnlyLocalAppletsKitMocks()).toEqual([]);
  });

  it("keeps reviewed local applets-kit mock overrides exact", () => {
    expect(findLocalAppletsKitMockOverrides()).toEqual(reviewedLocalAppletsKitMockOverrides);
  });

  it("keeps the shared applets-kit helper scoped to reviewed boundaries", () => {
    expect(findSharedAppletsKitMockOverrides()).toEqual(
      Array.from(sharedAppletsKitMockOverrides).sort(),
    );
  });

  it("keeps applets-kit Button real in tests", () => {
    expect(findDisallowedAppletsKitMockOverrides()).toEqual([]);
  });

  it("keeps explicit skipped tests reviewed", () => {
    expect(findExplicitSkips()).toEqual(reviewedExplicitSkips);
  });

  it("keeps fake timer tests restoring real timers", () => {
    expect(findFakeTimerCleanupViolations()).toEqual([]);
  });

  it("keeps hardcoded Paraglide-looking message literals reviewed", () => {
    expect(findHardcodedMessageLiteralCandidates()).toEqual(
      reviewedHardcodedMessageLiteralCandidates,
    );
  });

  it("keeps user index truthiness checks reviewed because index zero is valid", () => {
    expect(findUserIndexTruthinessChecks()).toEqual(reviewedUserIndexTruthinessChecks);
  });
});
