"use client";

import { useSettings } from "@/features/settings/store/SettingsStore";
import { useCallback, useEffect, useMemo, useState } from "react";
import {
  ArrowUpRight,
  Check,
  CircleAlert,
  Download,
  Github,
  RefreshCw,
  RotateCw,
  ShieldCheck,
} from "lucide-react";
import { useTranslation } from "react-i18next";

import { Toggle } from "@/components/settings/Toggle";
import { PRODUCT_DOWNLOADS_PATH, PRODUCT_RELEASES_URL, PRODUCT_REPOSITORY_URL } from "@/lib/product";
import { pageActionClass } from "@/components/layout/FeaturePage";
import {
  SettingRow,
  SettingSection,
  SettingsPageHeader,
} from "@/components/settings/shared";
import { ConfirmDialog } from "@/components/ui/ConfirmDialog";
import Button from "@/components/ui/Button";
import DesktopUpdatesPanel from "@/components/settings/DesktopUpdatesPanel";
import DockerUpdateProgressDialog from "@/components/settings/DockerUpdateProgressDialog";
import {
  checkAppUpdate,
  AppUpdateUnavailableError,
  fetchAppUpdateJob,
  fetchAppUpdateStatus,
  requestAppUpdate,
  setAppUpdateChecks,
  updateJobIsActive,
  type AppUpdateStatus,
  type InstallMode,
  type UpdateJob,
  type UpdateJobStatus,
} from "@/lib/app-update";
import {
  clientEdition,
  normalizeVersionTag,
  type ClientEdition,
} from "@/lib/version";

const POLL_INTERVAL_MS = 2_000;
const POLL_TIMEOUT_MS = 35 * 60_000;

const INSTALLATION_LABELS: Record<InstallMode, string> = {
  pypi: "PyPI package",
  source: "Source checkout",
  docker: "Docker container",
  unknown: "Unknown installation",
};

function jobTone(status: UpdateJobStatus) {
  if (status === "failed") return "text-red-600 dark:text-red-400";
  if (status === "succeeded") return "text-emerald-700 dark:text-emerald-400";
  return "text-sky-700 dark:text-sky-400";
}

export default function AboutSettingsPage() {
  const { t, i18n } = useTranslation();
  const [status, setStatus] = useState<AppUpdateStatus | null>(null);
  const [job, setJob] = useState<UpdateJob | null>(null);
  const [loading, setLoading] = useState(true);
  const [checking, setChecking] = useState(false);
  const { draftRevision } = useSettings();
  const [savingChecks, setSavingChecks] = useState(false);
  const [checks, setChecks] = useState({ enabled: false });
  const [requesting, setRequesting] = useState(false);
  const [confirmation, setConfirmation] = useState<{ target: string; revision?: string } | null>(null);
  const [progressOpen, setProgressOpen] = useState(false);
  const [updateNotice, setUpdateNotice] = useState("");
  const [error, setError] = useState("");
  const [unavailable, setUnavailable] = useState(false);
  const [desktopUpdates, setDesktopUpdates] = useState(false);
  useEffect(() => setDesktopUpdates(Boolean(window.loreviaUpdates)), []);

  const load = useCallback(async () => {
    try {
      const next = await fetchAppUpdateStatus();
      setStatus(next);
      setUnavailable(false);
      setChecks({ enabled: next.check_enabled });
      setJob(next.job);
      if (next.job && updateJobIsActive(next.job.status)) setProgressOpen(true);
      setError(next.check_error || "");
    } catch (cause) {
      if (cause instanceof AppUpdateUnavailableError) {
        setStatus(null);
        setJob(null);
        setUnavailable(true);
        setError("");
        return;
      }
      setUnavailable(false);
      setError(
        cause instanceof Error
          ? cause.message
          : (t("Unable to load version information.") as string),
      );
    } finally {
      setLoading(false);
    }
  }, [t]);

  useEffect(() => {
    void load();
  }, [load, draftRevision]);

  const activeJob = Boolean(job && updateJobIsActive(job.status));
  useEffect(() => {
    if (!activeJob) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const deadline = Date.now() + POLL_TIMEOUT_MS;

    const poll = async () => {
      try {
        const next = await fetchAppUpdateJob();
        if (cancelled) return;
        if (next) {
          setJob(next);
          if (!updateJobIsActive(next.status)) {
            if (next.status === "succeeded") await load();
            return;
          }
        }
      } catch {
        // The backend is expected to disappear while the launcher updates and
        // restarts it. Keep polling until it reconnects or the deadline passes.
      }
      if (cancelled) return;
      if (Date.now() >= deadline) {
        setError(t("DeepTutor did not reconnect before the update timeout."));
        return;
      }
      timer = setTimeout(poll, POLL_INTERVAL_MS);
    };

    void poll();
    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  }, [activeJob, load, t]);

  const check = useCallback(async () => {
    setChecking(true);
    setError("");
    setUpdateNotice("");
    try {
      const next = await checkAppUpdate();
      setStatus(next);
      setChecks({ enabled: next.check_enabled });
      setJob(next.job);
      if (next.installation.mode === "docker" && !next.update_available) {
        setUpdateNotice(t("The server already has the latest commit."));
      }
    } catch (cause) {
      setError(
        cause instanceof Error
          ? cause.message
          : (t("Unable to check for updates.") as string),
      );
    } finally {
      setChecking(false);
    }
  }, [t]);

  const changeChecks = async (enabled: boolean) => {
    setSavingChecks(true);
    setError("");
    try {
      const next = await setAppUpdateChecks(enabled);
      setStatus(next);
      setChecks({ enabled: next.check_enabled });
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("Unable to check for updates."));
    } finally {
      setSavingChecks(false);
    }
  };

  const prepareServerUpdate = useCallback(async () => {
    setChecking(true);
    setError("");
    setUpdateNotice("");
    try {
      const next = await checkAppUpdate();
      setStatus(next);
      setChecks({ enabled: next.check_enabled });
      setJob(next.job);
      if (next.update_available && next.release?.revision) {
        setConfirmation({ target: next.release.revision.slice(0, 12), revision: next.release.revision });
      } else if (!next.update_available) {
        setUpdateNotice(t("The server already has the latest commit."));
      } else {
        setError(t("No verified target commit was returned. Check the updater service."));
      }
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("Unable to check for updates."));
    } finally {
      setChecking(false);
    }
  }, [t]);

  const update = useCallback(async () => {
    if (!confirmation) return;
    setRequesting(true);
    setError("");
    try {
      const next = await requestAppUpdate(confirmation.revision);
      setJob(next);
      setProgressOpen(true);
      setConfirmation(null);
    } catch (cause) {
      setConfirmation(null);
      setError(
        cause instanceof Error
          ? cause.message
          : (t("Unable to start the update.") as string),
      );
    } finally {
      setRequesting(false);
    }
  }, [t, confirmation]);

  const currentVersion =
    normalizeVersionTag(status?.current_version) ??
    normalizeVersionTag(process.env.NEXT_PUBLIC_APP_VERSION) ??
    "—";
  const latestVersion = normalizeVersionTag(status?.release?.version);
  const serverUpdates = status?.installation.mode === "docker";
  const updateTarget = status?.release?.revision?.slice(0, 12) ?? latestVersion ?? "";
  // The user agent only exists in the browser; start from the server's view.
  const [edition, setEdition] = useState<ClientEdition>(() =>
    clientEdition(""),
  );
  useEffect(() => setEdition(clientEdition()), []);
  const checkedAt = useMemo(() => {
    if (!status?.checked_at) return "";
    const parsed = new Date(status.checked_at);
    if (Number.isNaN(parsed.getTime())) return "";
    return parsed.toLocaleString(
      i18n.language?.toLowerCase().startsWith("zh") ? "zh-CN" : "en-US",
      { dateStyle: "medium", timeStyle: "short" },
    );
  }, [i18n.language, status?.checked_at]);

  const installMode = status?.installation.mode ?? "unknown";
  const canUpdate = Boolean(
    status?.is_admin &&
    status.check_enabled &&
    status.update_available &&
    status.installation.automatic_update &&
    status.launcher_managed &&
    (!serverUpdates || status.release?.revision) && !error && !checking && !requesting && !activeJob,
  );
  const canStartServerUpdate = Boolean(
    serverUpdates && status?.is_admin && status.check_enabled &&
    status.installation.automatic_update && status.launcher_managed &&
    !checking && !requesting && !activeJob && !savingChecks,
  );
  const upToDate = Boolean(status?.release && !status.update_available);

  return (
    <div data-tour="tour-about" className="pb-8">
      <SettingsPageHeader
        title={t("About")}
        description={t(
          "DeepTutor version, release channel, and the safest update path for this installation.",
        )}
      />

      <section className="relative mb-10 overflow-hidden border-y border-[var(--border)]/60 py-7">
        <div
          aria-hidden
          className="pointer-events-none absolute -right-12 -top-16 h-40 w-40 rounded-full border-[28px] border-[var(--foreground)]/[0.025]"
        />
        <div className="relative flex flex-col gap-5 sm:flex-row sm:items-end sm:justify-between">
          <div>
            <div className="mb-2 flex items-center gap-2 text-[11px] font-medium uppercase tracking-[0.16em] text-[var(--muted-foreground)]">
              <span className="h-1.5 w-1.5 rounded-full bg-emerald-500" />
              {t("Running version")}
            </div>
            <div className="font-serif text-[38px] font-semibold leading-none tracking-[-0.035em] text-[var(--foreground)]">
              {edition.short} {currentVersion}
            </div>
            <div className="mt-2 text-[13px] text-[var(--muted-foreground)]">
              {edition.full} {currentVersion}
            </div>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <a
              href={PRODUCT_RELEASES_URL}
              target="_blank"
              rel="noreferrer noopener"
              className={pageActionClass("secondary")}
            >
              <RefreshCw className="h-3.5 w-3.5" />
              {t("View updates")}
            </a>
            {!serverUpdates && !canUpdate && (
              <a
                href={PRODUCT_DOWNLOADS_PATH}
                target="_blank"
                rel="noreferrer noopener"
                className={pageActionClass()}
              >
                <Download className="h-3.5 w-3.5" />
                {t("Download desktop app")}
              </a>
            )}
            {status?.release?.url && (
              <a
                href={status.release.url}
                target="_blank"
                rel="noreferrer noopener"
                className="inline-flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-[12px] font-medium text-[var(--muted-foreground)] transition-colors hover:bg-[var(--accent)] hover:text-[var(--foreground)]"
              >
                {t("Release notes")}
                <ArrowUpRight className="h-3.5 w-3.5" />
              </a>
            )}
          </div>
        </div>
        {unavailable && edition.short !== "Web" && (
          <p role="status" className="relative mt-4 text-[13px] leading-relaxed text-[var(--muted-foreground)]">
            {desktopUpdates
              ? t("Check for signed core updates from GitHub below. Use the desktop installer to upgrade the desktop shell.")
              : t("Update this desktop app by downloading an installer for your platform.")}
          </p>
        )}
      </section>

      {error && (
        <div className="mb-7 flex items-start gap-2 border-l-2 border-red-500/70 py-1 pl-3 text-[12.5px] leading-relaxed text-red-600 dark:text-red-400">
          <CircleAlert className="mt-0.5 h-4 w-4 shrink-0" />
          <span>{error}</span>
        </div>
      )}

      {status && <SettingSection
        title={t(serverUpdates ? "Docker updates" : "Updates")}
        description={t(serverUpdates
          ? "Check for server code changes, then confirm to rebuild and restart. Building may take several minutes."
          : "Version checks are cached for 24 hours. DeepTutor never installs an update without confirmation.")}
      >
        {serverUpdates && (
          <>
            <SettingRow title={t("Deployed commit")} control={<CodeValue value={status.current_revision?.slice(0, 12) || "—"} />} />
            {status.source && <SettingRow title={t("Update source")} control={
              <div className="flex flex-wrap items-center gap-2">
                {status.source.repository && <a className="text-[13px] underline" href={status.source.repository} target="_blank" rel="noopener noreferrer">
                  {status.source.repository.replace("https://github.com/", "")}
                </a>}
                <CodeValue value={status.source.branch} />
              </div>
            } />}
          </>
        )}
        <SettingRow
          title={t("Check for updates")}
          description={t(
            "Turn this off for offline or controlled deployments.",
          )}
          control={
            status?.is_admin ? (
              <Toggle
                checked={checks.enabled}
                disabled={loading || savingChecks || activeJob}
                onChange={(enabled) => void changeChecks(enabled)}
              />
            ) : (
              <span className="text-[12.5px] text-[var(--muted-foreground)]">
                {status?.check_enabled ? t("On") : t("Off")}
              </span>
            )
          }
        />
        <SettingRow
          title={t(serverUpdates ? "Latest server code" : "Latest stable release")}
          description={
            checkedAt
              ? t("Last checked {{time}}", { time: checkedAt })
              : status?.check_enabled
                ? t("Not checked yet")
                : t("Version checks are disabled.")
          }
          control={
            <div className="flex items-center gap-2">
              {upToDate && <Check className="h-3.5 w-3.5 text-emerald-500" />}
              <CodeValue value={status?.release?.revision?.slice(0, 12) ?? latestVersion ?? "—"} />
            </div>
          }
        />

        <div className="flex flex-wrap gap-2 border-t border-[var(--border)]/50 py-4">
          {status?.is_admin && (
            <Button
              type="button"
              size="sm"
              variant="secondary"
              loading={checking}
              disabled={!status.check_enabled || activeJob || savingChecks || requesting}
              icon={<RefreshCw className="h-3.5 w-3.5" />}
              onClick={() => void check()}
            >
              {t(serverUpdates ? "Check for new commits" : "Check now")}
            </Button>
          )}
          {(canStartServerUpdate || (!serverUpdates && canUpdate)) && (
            <Button
              type="button"
              size="sm"
              className={serverUpdates ? "bg-emerald-600 text-white hover:bg-emerald-700 active:bg-emerald-800" : undefined}
              icon={<Download className="h-3.5 w-3.5" />}
              onClick={() => serverUpdates
                ? void prepareServerUpdate()
                : setConfirmation({ target: updateTarget, revision: status.release?.revision })}
            >
              {serverUpdates ? t("Start update") : t("Update to {{version}}", { version: updateTarget })}
            </Button>
          )}
        </div>
        {serverUpdates && updateNotice && <p role="status" className="pb-4 text-xs text-emerald-700 dark:text-emerald-400">{updateNotice}</p>}

        {status?.release?.migration_warning && (
          <div className="flex items-start gap-2 border-t border-[var(--border)]/50 py-3.5 text-[12.5px] leading-relaxed text-amber-700 dark:text-amber-400">
            <CircleAlert className="mt-0.5 h-4 w-4 shrink-0" />
            {t(
              "This release mentions migrations or breaking changes. Read the release notes before updating.",
            )}
          </div>
        )}

        {!serverUpdates && status?.release?.excerpt && (
          <div className="border-t border-[var(--border)]/50 py-4">
            <div className="mb-2 text-[11px] font-medium uppercase tracking-[0.14em] text-[var(--muted-foreground)]">
              {status.release.name || t("What changed")}
            </div>
            <p className="max-w-2xl whitespace-pre-line text-[12.5px] leading-6 text-[var(--muted-foreground)]">
              {status.release.excerpt}
            </p>
          </div>
        )}

        {status && !status.installation.automatic_update && (
          <div className="border-t border-[var(--border)]/50 py-4">
            <div className="mb-2 flex items-center gap-2 text-[12.5px] font-medium text-[var(--foreground)]">
              <ShieldCheck className="h-4 w-4 text-[var(--muted-foreground)]" />
              {t("Managed by your installation")}
            </div>
            <p className="mb-3 text-[12.5px] leading-relaxed text-[var(--muted-foreground)]">
              {t(status.installation.reason)}
            </p>
            <code className="block overflow-x-auto rounded-lg bg-[var(--accent)]/60 px-3 py-2 text-[11.5px] text-[var(--foreground)]">
              {status.installation.command}
            </code>
          </div>
        )}

        {job && (
          <div
            className="border-t border-[var(--border)]/50 py-4"
            aria-live="polite"
          >
            <div
              className={`flex items-center gap-2 text-[12.5px] font-medium ${jobTone(job.status)}`}
            >
              {updateJobIsActive(job.status) ? (
                <RotateCw className="h-3.5 w-3.5 animate-spin" />
              ) : job.status === "succeeded" ? (
                <Check className="h-3.5 w-3.5" />
              ) : (
                <CircleAlert className="h-3.5 w-3.5" />
              )}
              {t(`updateJob.${job.status}`)}
            </div>
            {job.error && (
              <p className="mt-2 text-[12px] text-red-600 dark:text-red-400">
                {job.error}
              </p>
            )}
            {serverUpdates && <button type="button" onClick={() => setProgressOpen(true)} className="mt-2 text-xs font-medium text-emerald-700 underline dark:text-emerald-400">{t("View deployment progress")}</button>}
          </div>
        )}
      </SettingSection>}

      {!status && !loading && edition.short === "Web" && (
        <SettingSection title={t("Docker updates")} description={unavailable
          ? t("Enable the Docker updater on your server to pull new commits from GitHub here.")
          : t("Could not read the server update status.")}>
          {unavailable && <ResourceRow
            title={t("Set up Docker updates")}
            description={t("Configure the GitHub checkout and the update service on the Docker host.")}
            href={`${PRODUCT_REPOSITORY_URL}/blob/master/deployment/README.md`}
            icon={<ArrowUpRight className="h-4 w-4" />}
          />}
          <Button type="button" size="sm" variant="secondary" icon={<RefreshCw className="h-3.5 w-3.5" />}
            onClick={() => void load()}>{t("Refresh status")}</Button>
        </SettingSection>
      )}

      <DesktopUpdatesPanel />

      <SettingSection title={t("Download desktop app")}>
        <ResourceRow
          title={t("Download desktop app")}
          description={t("Check available installers for macOS Apple Silicon, macOS Intel and Windows x64.")}
          href={PRODUCT_DOWNLOADS_PATH}
          icon={<Download className="h-4 w-4" />}
        />
      </SettingSection>

      <SettingSection title={t("Software")}>
        <SettingRow
          title={t("Current version")}
          control={<CodeValue value={currentVersion} />}
        />
        {status && <SettingRow
          title={t("Installation")}
          description={t(
            status?.installation.reason || "How DeepTutor is installed here.",
          )}
          control={
            <span className="text-[12.5px] text-[var(--foreground)]">
              {t(INSTALLATION_LABELS[installMode])}
            </span>
          }
        />}
        {status && <SettingRow
          title={t("Release channel")}
          description={t(serverUpdates
            ? "Checks the server's configured Git tracking branch."
            : "Only stable, published DeepTutor releases are considered.")}
          control={
            <span className="text-[12.5px] text-[var(--foreground)]">
              {t(serverUpdates ? "Server Git branch" : "Stable")}
            </span>
          }
        />}
      </SettingSection>

      <SettingSection title={t("Project")}>
        <ResourceRow
          title={t("GitHub")}
          description={t("Source code, issues, and contributions")}
          href={PRODUCT_REPOSITORY_URL}
          icon={<Github className="h-4 w-4" />}
        />
        <ResourceRow
          title={t("Documentation")}
          description={t("Installation, configuration, and guides")}
          href={`${PRODUCT_REPOSITORY_URL}#快速开始`}
          icon={<ArrowUpRight className="h-4 w-4" />}
        />
      </SettingSection>

      <ConfirmDialog
        open={Boolean(confirmation)}
        title={t("Update and restart DeepTutor?")}
        confirmLabel={t(serverUpdates ? "Pull code and rebuild" : "Update and restart")}
        busy={requesting}
        busyLabel={t("Preparing update…")}
        onConfirm={() => void update()}
        onCancel={() => setConfirmation(null)}
      >
        {t(serverUpdates
          ? "The server will pull code, build an image and restart the application. Finish active conversations first. Settings and data are preserved. Target: {{version}}."
          : "DeepTutor will briefly stop, install {{version}}, and reopen with the same settings. Active conversations must finish first.",
          { version: confirmation?.target })}
      </ConfirmDialog>
      {progressOpen && <DockerUpdateProgressDialog job={job} branch={status?.source?.branch} onClose={() => setProgressOpen(false)} />}
    </div>
  );
}

function CodeValue({ value }: { value: string }) {
  return (
    <code className="rounded-md bg-[var(--accent)]/60 px-2 py-1 text-[11.5px] tabular-nums text-[var(--foreground)]">
      {value}
    </code>
  );
}

function ResourceRow({
  title,
  description,
  href,
  icon,
}: {
  title: string;
  description: string;
  href: string;
  icon: React.ReactNode;
}) {
  return (
    <a
      href={href}
      target="_blank"
      rel="noreferrer noopener"
      className="group flex items-center justify-between gap-5 border-t border-[var(--border)]/50 py-3.5 first:border-t-0"
    >
      <div>
        <div className="text-[13.5px] font-medium text-[var(--foreground)]">
          {title}
        </div>
        <p className="mt-1 text-[12px] text-[var(--muted-foreground)]">
          {description}
        </p>
      </div>
      <span className="text-[var(--muted-foreground)] transition-transform group-hover:-translate-y-0.5 group-hover:translate-x-0.5 group-hover:text-[var(--foreground)]">
        {icon}
      </span>
    </a>
  );
}
