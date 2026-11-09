"use client";

import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import Button from "@/components/ui/Button";
import { SettingSection } from "@/components/settings/shared";
import { PRODUCT_RELEASES_URL } from "@/lib/product";

interface DesktopUpdate {
  version: string;
  available: boolean;
  compatible: boolean;
  url: string;
}

declare global {
  interface Window {
    loreviaUpdates?: {
      check(): Promise<DesktopUpdate>;
      install(version: string): Promise<void>;
    };
  }
}

export default function DesktopUpdatesPanel() {
  const { t } = useTranslation();
  const [desktop, setDesktop] = useState(false);
  const [update, setUpdate] = useState<DesktopUpdate | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => setDesktop(Boolean(window.loreviaUpdates)), []);
  if (!desktop) return null;

  const check = async () => {
    setBusy(true);
    setError("");
    try { setUpdate(await window.loreviaUpdates!.check()); }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)); }
    finally { setBusy(false); }
  };
  const install = async () => {
    if (!update) return;
    setBusy(true);
    setError("");
    try { await window.loreviaUpdates!.install(update.version); }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)); }
    finally { setBusy(false); }
  };
  return (
    <SettingSection title={t("GitHub desktop updates")} description={t("Download signed core updates from GitHub Releases. Your data is backed up before installation, and a failed update restores the previous core. Use the latest installer to update the desktop shell.")}>
      <div className="flex flex-wrap items-center gap-3 py-3">
        <Button size="sm" variant="secondary" disabled={busy} onClick={() => void check()}>{t("Check now")}</Button>
        {update && <span role="status" className="text-sm">{update.available ? t("Update to {{version}}", { version: `v${update.version}` }) : t("The core is up to date.")}</span>}
        {update?.available && update.compatible && <Button size="sm" loading={busy} disabled={busy} onClick={() => void install()}>{t("Download and install core update")}</Button>}
        {update?.available && !update.compatible && <a className="text-sm underline" href={PRODUCT_RELEASES_URL + "/latest"} target="_blank" rel="noopener noreferrer">{t("Download desktop app")}</a>}
      </div>
      {busy && <p role="status" className="text-sm text-[var(--muted-foreground)]">{t("Checking or downloading the GitHub update…")}</p>}
      {error && <p role="alert" className="text-sm text-red-600 dark:text-red-400">{error}</p>}
    </SettingSection>
  );
}
