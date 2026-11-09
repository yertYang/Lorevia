"use client";

import { Check, CircleAlert, RefreshCw, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { UpdateJob } from "@/lib/app-update";

const stages = [
  { key: "checking", progress: 8 },
  { key: "pulling", progress: 22 },
  { key: "building", progress: 38 },
  { key: "deploying", progress: 76 },
  { key: "verifying", progress: 90 },
] as const;

export default function DockerUpdateProgressDialog({
  job,
  branch,
  onClose,
}: {
  job: UpdateJob | null;
  branch?: string;
  onClose: () => void;
}) {
  const { t, i18n } = useTranslation();
  if (!job) return null;
  const done = job.status === "succeeded";
  const failed = job.status === "failed";
  const stage = failed ? "failed" : job.stage ?? (done ? "complete" : "checking");
  const progress = Math.max(0, Math.min(100, job.progress ?? (done ? 100 : 5)));
  const started = new Date(job.started_at ?? job.created_at);
  const startedLabel = Number.isNaN(started.getTime()) ? "—" : started.toLocaleString(
    i18n.language?.startsWith("zh") ? "zh-CN" : "en-US",
    { dateStyle: "medium", timeStyle: "short" },
  );

  return (
    <div className="fixed inset-0 z-[70] flex items-center justify-center bg-black/45 p-3 sm:p-6" role="dialog" aria-modal="true" aria-label={t("System update progress")}>
      <section className="flex max-h-[min(90vh,850px)] w-full max-w-2xl flex-col overflow-hidden rounded-2xl border border-[var(--border)] bg-[var(--card)] text-[var(--foreground)] shadow-2xl">
        <header className="flex items-start justify-between gap-4 border-b border-[var(--border)] px-5 py-4 sm:px-6">
          <div className="flex min-w-0 items-center gap-3">
            <span className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-emerald-500/10 text-emerald-600 dark:text-emerald-400">
              {done ? <Check size={21} /> : failed ? <CircleAlert size={21} /> : <RefreshCw size={20} className="animate-spin" />}
            </span>
            <div>
              <h2 className="text-lg font-semibold">{t("System update")}</h2>
              <p className="text-xs text-[var(--muted-foreground)]">{t("The deployment continues if you close this window.")}</p>
            </div>
          </div>
          <button type="button" onClick={onClose} aria-label={t("Close")} className="rounded-lg p-1.5 text-[var(--muted-foreground)] hover:bg-[var(--accent)] hover:text-[var(--foreground)]"><X size={18} /></button>
        </header>

        <div className="overflow-y-auto p-5 sm:p-6">
          <div className="rounded-xl bg-[color-mix(in_srgb,var(--muted)_45%,transparent)] p-4">
            <div className="flex items-center justify-between gap-3">
              <div>
                <p className="text-xs text-[var(--muted-foreground)]">{t("Application update")}</p>
                <h3 className="mt-1 font-semibold">{t("Docker source deployment")}</h3>
              </div>
              <span className={`rounded-full px-2.5 py-1 text-xs font-medium ${failed ? "bg-red-500/10 text-red-600" : done ? "bg-emerald-500/10 text-emerald-700 dark:text-emerald-400" : "bg-sky-500/10 text-sky-700 dark:text-sky-400"}`}>
                {t(failed ? "Update failed" : done ? "Update complete" : "Updating")}
              </span>
            </div>
            <dl className="mt-4 grid grid-cols-2 gap-2 sm:grid-cols-3">
              {[
                [t("Update source"), branch || "GitHub"],
                [t("Target commit"), job.target_version?.slice(0, 12) || "—"],
                [t("Started at"), startedLabel],
              ].map(([label, value], index) => <div key={label} className={`min-w-0 rounded-lg bg-[var(--card)] px-3 py-2.5 ${index === 2 ? "col-span-2 sm:col-span-1" : ""}`}><dt className="text-[11px] text-[var(--muted-foreground)]">{label}</dt><dd className="mt-1 truncate text-xs font-medium" title={value}>{value}</dd></div>)}
            </dl>
          </div>

          <div className="mt-4 rounded-xl bg-[color-mix(in_srgb,var(--muted)_45%,transparent)] p-4" aria-live="polite">
            <div className="flex items-center justify-between gap-3 text-sm font-medium"><span>{t("Update progress")}</span><span>{progress}%</span></div>
            <p className="mt-3 text-xs text-[var(--muted-foreground)]">{t(`updateStage.${stage}`)}</p>
            <div className="mt-3 h-2 overflow-hidden rounded-full bg-[var(--border)]" role="progressbar" aria-valuenow={progress} aria-valuemin={0} aria-valuemax={100} aria-label={t("Update progress")}>
              <div className={`h-full rounded-full transition-[width] duration-500 ${failed ? "bg-red-500" : "bg-emerald-600"}`} style={{ width: `${progress}%` }} />
            </div>
            <p className="mt-2 text-[11px] text-[var(--muted-foreground)]">{t("Progress follows deployment stages; image build time may vary.")}</p>
            <ol className="mt-4 space-y-2 border-t border-[var(--border)] pt-4">
              {stages.map((item) => {
                const reached = progress >= item.progress;
                return <li key={item.key} className={`flex items-center gap-2 text-xs ${reached ? "text-[var(--foreground)]" : "text-[var(--muted-foreground)]"}`}>
                  <span className={`h-1.5 w-1.5 rounded-full ${reached ? "bg-emerald-600" : "bg-[var(--border)]"}`} />{t(`updateStage.${item.key}`)}
                </li>;
              })}
            </ol>
          </div>
          {failed && job.error && <p className="mt-4 rounded-lg bg-red-500/10 px-3 py-2.5 text-sm text-red-700 dark:text-red-300" role="alert">{job.error}</p>}
          {job.events && job.events.length > 0 && <div className="mt-4 text-xs text-[var(--muted-foreground)]"><h4 className="mb-2 font-medium text-[var(--foreground)]">{t("Recent activity")}</h4><ol className="space-y-1.5">{job.events.slice(-5).map((event, index) => <li key={`${event.at}-${index}`} className="flex gap-3"><time className="shrink-0">{new Date(event.at).toLocaleTimeString()}</time><span>{t(`updateEvent.${event.stage}`)}</span></li>)}</ol></div>}
        </div>
        <footer className="flex items-center justify-between gap-3 border-t border-[var(--border)] px-5 py-4 text-xs text-[var(--muted-foreground)] sm:px-6">
          <span>{done ? t("The new version is healthy.") : failed ? t("The update needs attention.") : t("Keep this page open for live progress.")}</span>
          <button type="button" onClick={onClose} className="rounded-lg bg-[var(--accent)] px-3 py-2 font-medium text-[var(--foreground)] hover:opacity-80">{t(done || failed ? "Close" : "Continue in background")}</button>
        </footer>
      </section>
    </div>
  );
}
