import type { TFunction } from "i18next";

export const STARTER_GROUPS = ["explain", "solve", "practice", "plan", "research"] as const;
export type StarterGroup = (typeof STARTER_GROUPS)[number];
export interface StarterPrompt {
  label: string;
  prompt: string;
}

export function starterPrompts(group: StarterGroup, t: TFunction): StarterPrompt[] {
  return [1, 2, 3, 4].map((n) => ({
    label: t(`starters.${group}.${n}`),
    prompt: t(`starters.${group}.${n}.prompt`),
  }));
}
