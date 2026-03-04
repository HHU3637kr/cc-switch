import { useCallback, useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import {
  Link2,
  UploadCloud,
  DownloadCloud,
  Loader2,
  Save,
  Check,
  AlertTriangle,
  Github,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { useTranslation } from "react-i18next";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { settingsApi } from "@/lib/api";
import type { RemoteSnapshotInfo, GitHubSyncSettings } from "@/types";

// ─── Types ──────────────────────────────────────────────────

type ActionState =
  | "idle"
  | "testing"
  | "saving"
  | "uploading"
  | "downloading"
  | "fetching_remote";

type DialogType = "upload" | "download" | null;

interface GitHubSyncSectionProps {
  config?: GitHubSyncSettings;
}

/** Format an RFC 3339 date string for display; falls back to raw string. */
function formatDate(rfc3339: string): string {
  const d = new Date(rfc3339);
  return Number.isNaN(d.getTime()) ? rfc3339 : d.toLocaleString();
}

// ─── ActionButton ───────────────────────────────────────────

/** Reusable button with loading spinner. */
function ActionButton({
  actionState,
  targetState,
  alsoActiveFor,
  icon: Icon,
  activeLabel,
  idleLabel,
  disabled,
  ...props
}: {
  actionState: ActionState;
  targetState: ActionState;
  alsoActiveFor?: ActionState[];
  icon: LucideIcon;
  activeLabel: ReactNode;
  idleLabel: ReactNode;
} & Omit<React.ComponentPropsWithoutRef<typeof Button>, "children">) {
  const isActive =
    actionState === targetState ||
    (alsoActiveFor?.includes(actionState) ?? false);
  return (
    <Button {...props} disabled={actionState !== "idle" || disabled}>
      <span className="inline-flex items-center gap-2">
        {isActive ? (
          <Loader2 className="h-3.5 w-3.5 animate-spin" />
        ) : (
          <Icon className="h-3.5 w-3.5" />
        )}
        {isActive ? activeLabel : idleLabel}
      </span>
    </Button>
  );
}

// ─── Main component ─────────────────────────────────────────

export function GitHubSyncSection({ config }: GitHubSyncSectionProps) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [actionState, setActionState] = useState<ActionState>("idle");
  const [dirty, setDirty] = useState(false);
  const [tokenTouched, setTokenTouched] = useState(false);
  const [justSaved, setJustSaved] = useState(false);
  const justSavedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Local form state — credentials are only persisted on explicit "Save".
  const [form, setForm] = useState(() => ({
    token: config?.token ?? "",
    repo: config?.repo ?? "",
    branch: config?.branch ?? "main",
    remoteRoot: config?.remoteRoot ?? "cc-switch-sync",
    profile: config?.profile ?? "default",
    autoSync: config?.autoSync ?? false,
  }));

  // Confirmation dialog state
  const [dialogType, setDialogType] = useState<DialogType>(null);
  const [remoteInfo, setRemoteInfo] = useState<RemoteSnapshotInfo | null>(null);

  const closeDialog = useCallback(() => {
    setDialogType(null);
    setRemoteInfo(null);
  }, []);

  // Cleanup justSaved timer on unmount
  useEffect(() => {
    return () => {
      if (justSavedTimerRef.current) clearTimeout(justSavedTimerRef.current);
    };
  }, []);

  // Sync form when config is loaded/updated from backend, but not while user is editing
  useEffect(() => {
    if (!config || dirty) return;
    setForm({
      token: config.token ?? "",
      repo: config.repo ?? "",
      branch: config.branch ?? "main",
      remoteRoot: config.remoteRoot ?? "cc-switch-sync",
      profile: config.profile ?? "default",
      autoSync: config.autoSync ?? false,
    });
    setTokenTouched(false);
  }, [config, dirty]);

  const updateField = useCallback((field: keyof typeof form, value: string) => {
    setForm((prev) => ({ ...prev, [field]: value }));
    if (field === "token") {
      setTokenTouched(true);
    }
    setDirty(true);
    setJustSaved(false);
    if (justSavedTimerRef.current) {
      clearTimeout(justSavedTimerRef.current);
      justSavedTimerRef.current = null;
    }
  }, []);

  const handleAutoSyncChange = useCallback((checked: boolean) => {
    setForm((prev) => ({ ...prev, autoSync: checked }));
    setDirty(true);
    setJustSaved(false);
    if (justSavedTimerRef.current) {
      clearTimeout(justSavedTimerRef.current);
      justSavedTimerRef.current = null;
    }
  }, []);

  const buildSettings = useCallback((): GitHubSyncSettings | null => {
    const repo = form.repo.trim();
    if (!repo) return null;
    return {
      enabled: true,
      token: form.token,
      repo,
      branch: form.branch.trim() || "main",
      remoteRoot: form.remoteRoot.trim() || "cc-switch-sync",
      profile: form.profile.trim() || "default",
      autoSync: form.autoSync,
    };
  }, [form]);

  // ─── Handlers ───────────────────────────────────────────

  const handleTest = useCallback(async () => {
    const settings = buildSettings();
    if (!settings) {
      toast.error(t("settings.githubSync.missingRepo"));
      return;
    }
    setActionState("testing");
    try {
      await settingsApi.githubTestConnection(settings, !tokenTouched);
      toast.success(t("settings.githubSync.testSuccess"));
    } catch (error) {
      toast.error(
        t("settings.githubSync.testFailed", {
          error: (error as Error)?.message ?? String(error),
        }),
      );
    } finally {
      setActionState("idle");
    }
  }, [buildSettings, tokenTouched, t]);

  const handleSave = useCallback(async () => {
    const settings = buildSettings();
    if (!settings) {
      toast.error(t("settings.githubSync.missingRepo"));
      return;
    }
    setActionState("saving");
    try {
      await settingsApi.githubSyncSaveSettings(settings, tokenTouched);
      setDirty(false);
      setTokenTouched(false);
      // Show "saved" indicator for 2 seconds
      setJustSaved(true);
      if (justSavedTimerRef.current) clearTimeout(justSavedTimerRef.current);
      justSavedTimerRef.current = setTimeout(() => {
        setJustSaved(false);
        justSavedTimerRef.current = null;
      }, 2000);
      await queryClient.invalidateQueries();
    } catch (error) {
      toast.error(
        t("settings.githubSync.saveFailed", {
          error: (error as Error)?.message ?? String(error),
        }),
      );
      setActionState("idle");
      return;
    }

    // Auto-test connection after save
    setActionState("testing");
    try {
      await settingsApi.githubTestConnection(settings, true);
      toast.success(t("settings.githubSync.saveAndTestSuccess"));
    } catch (error) {
      toast.warning(
        t("settings.githubSync.saveAndTestFailed", {
          error: (error as Error)?.message ?? String(error),
        }),
      );
    } finally {
      setActionState("idle");
    }
  }, [buildSettings, tokenTouched, queryClient, t]);

  /** Fetch remote info, then open upload confirmation dialog. */
  const handleUploadClick = useCallback(async () => {
    if (dirty) {
      toast.error(t("settings.githubSync.unsavedChanges"));
      return;
    }
    setActionState("fetching_remote");
    try {
      const info = await settingsApi.githubSyncFetchRemoteInfo();
      if ("empty" in info) {
        setRemoteInfo(null);
      } else {
        setRemoteInfo(info);
      }
      setDialogType("upload");
    } catch {
      setRemoteInfo(null);
      toast.error(t("settings.githubSync.fetchRemoteFailed"));
      setActionState("idle");
      return;
    }
    setActionState("idle");
  }, [dirty, t]);

  /** Actually perform the upload after user confirms. */
  const handleUploadConfirm = useCallback(async () => {
    if (dirty) {
      toast.error(t("settings.githubSync.unsavedChanges"));
      return;
    }
    closeDialog();
    setActionState("uploading");
    try {
      await settingsApi.githubSyncUpload();
      toast.success(t("settings.githubSync.uploadSuccess"));
      await queryClient.invalidateQueries();
    } catch (error) {
      toast.error(
        t("settings.githubSync.uploadFailed", {
          error: (error as Error)?.message ?? String(error),
        }),
      );
    } finally {
      setActionState("idle");
    }
  }, [closeDialog, dirty, queryClient, t]);

  /** Fetch remote info, then open download confirmation dialog. */
  const handleDownloadClick = useCallback(async () => {
    if (dirty) {
      toast.error(t("settings.githubSync.unsavedChanges"));
      return;
    }
    setActionState("fetching_remote");
    try {
      const info = await settingsApi.githubSyncFetchRemoteInfo();
      if ("empty" in info) {
        toast.info(t("settings.githubSync.noRemoteData"));
        return;
      }
      if (!info.compatible) {
        toast.error(
          t("settings.githubSync.incompatibleVersion", {
            version: info.version,
          }),
        );
        return;
      }
      setRemoteInfo(info);
      setDialogType("download");
    } catch (error) {
      toast.error(
        t("settings.githubSync.downloadFailed", {
          error: (error as Error)?.message ?? String(error),
        }),
      );
    } finally {
      setActionState("idle");
    }
  }, [dirty, t]);

  /** Actually perform the download after user confirms. */
  const handleDownloadConfirm = useCallback(async () => {
    if (dirty) {
      toast.error(t("settings.githubSync.unsavedChanges"));
      return;
    }
    closeDialog();
    setActionState("downloading");
    try {
      await settingsApi.githubSyncDownload();
      toast.success(t("settings.githubSync.downloadSuccess"));
      await queryClient.invalidateQueries();
    } catch (error) {
      toast.error(
        t("settings.githubSync.downloadFailed", {
          error: (error as Error)?.message ?? String(error),
        }),
      );
    } finally {
      setActionState("idle");
    }
  }, [closeDialog, dirty, queryClient, t]);

  // ─── Derived state ──────────────────────────────────────

  const isLoading = actionState !== "idle";
  const hasSavedConfig = Boolean(config?.repo?.trim()) && !dirty;

  const lastSyncAt = config?.status?.lastSyncAt;
  const lastSyncDisplay = lastSyncAt
    ? new Date(lastSyncAt * 1000).toLocaleString()
    : null;
  const lastError = config?.status?.lastError?.trim();
  const showAutoSyncError =
    !!lastError && config?.status?.lastErrorSource === "auto";

  // ─── Render ─────────────────────────────────────────────

  return (
    <section className="space-y-4">
      <header className="space-y-2">
        <h3 className="text-base font-semibold text-foreground flex items-center gap-2">
          <Github className="h-4 w-4" />
          {t("settings.githubSync.title")}
        </h3>
        <p className="text-sm text-muted-foreground">
          {t("settings.githubSync.description")}
        </p>
      </header>

      <div className="space-y-4 rounded-lg border border-border bg-muted/40 p-6">
        {/* Config fields */}
        <div className="space-y-3">
          {/* Token */}
          <div className="flex items-center gap-4">
            <label className="w-40 text-xs font-medium text-foreground shrink-0">
              {t("settings.githubSync.token")}
              <span className="block text-[10px] font-normal text-muted-foreground">
                {t("settings.githubSync.tokenHint")}
              </span>
            </label>
            <Input
              type="password"
              value={form.token}
              onChange={(e) => updateField("token", e.target.value)}
              placeholder={t("settings.githubSync.tokenPlaceholder")}
              className="text-xs flex-1"
              autoComplete="off"
              disabled={isLoading}
            />
          </div>

          {/* Repository */}
          <div className="flex items-center gap-4">
            <label className="w-40 text-xs font-medium text-foreground shrink-0">
              {t("settings.githubSync.repo")}
            </label>
            <Input
              value={form.repo}
              onChange={(e) => updateField("repo", e.target.value)}
              placeholder={t("settings.githubSync.repoPlaceholder")}
              className="text-xs flex-1"
              disabled={isLoading}
            />
          </div>

          {/* Branch */}
          <div className="flex items-center gap-4">
            <label className="w-40 text-xs font-medium text-foreground shrink-0">
              {t("settings.githubSync.branch")}
            </label>
            <Input
              value={form.branch}
              onChange={(e) => updateField("branch", e.target.value)}
              placeholder="main"
              className="text-xs flex-1"
              disabled={isLoading}
            />
          </div>

          {/* Remote Root */}
          <div className="flex items-center gap-4">
            <label className="w-40 text-xs font-medium text-foreground shrink-0">
              {t("settings.githubSync.remoteRoot")}
              <span className="block text-[10px] font-normal text-muted-foreground">
                {t("settings.githubSync.remoteRootDefault")}
              </span>
            </label>
            <Input
              value={form.remoteRoot}
              onChange={(e) => updateField("remoteRoot", e.target.value)}
              placeholder="cc-switch-sync"
              className="text-xs flex-1"
              disabled={isLoading}
            />
          </div>

          {/* Profile */}
          <div className="flex items-center gap-4">
            <label className="w-40 text-xs font-medium text-foreground shrink-0">
              {t("settings.githubSync.profile")}
              <span className="block text-[10px] font-normal text-muted-foreground">
                {t("settings.githubSync.profileDefault")}
              </span>
            </label>
            <Input
              value={form.profile}
              onChange={(e) => updateField("profile", e.target.value)}
              placeholder="default"
              className="text-xs flex-1"
              disabled={isLoading}
            />
          </div>

          <div className="flex items-start gap-4">
            <label className="w-40 text-xs font-medium text-foreground shrink-0">
              {t("settings.githubSync.autoSync")}
              <span className="block text-[10px] font-normal text-muted-foreground">
                {t("settings.githubSync.autoSyncHint")}
              </span>
            </label>
            <div className="pt-1">
              <Switch
                checked={form.autoSync}
                onCheckedChange={handleAutoSyncChange}
                aria-label={t("settings.githubSync.autoSync")}
                disabled={isLoading}
              />
            </div>
          </div>
        </div>

        {/* Last sync time */}
        {lastSyncDisplay && (
          <p className="text-xs text-muted-foreground">
            {t("settings.githubSync.lastSync", { time: lastSyncDisplay })}
          </p>
        )}
        {showAutoSyncError && (
          <div className="rounded-lg border border-red-300/70 bg-red-50/80 px-3 py-2 text-xs text-red-900 dark:border-red-500/50 dark:bg-red-950/30 dark:text-red-200">
            <p className="font-medium">
              {t("settings.githubSync.autoSyncLastErrorTitle")}
            </p>
            <p className="mt-1 break-all whitespace-pre-wrap">{lastError}</p>
            <p className="mt-1 text-[11px] text-red-700/90 dark:text-red-300/80">
              {t("settings.githubSync.autoSyncLastErrorHint")}
            </p>
          </div>
        )}

        {/* Config buttons + save status */}
        <div className="flex flex-wrap items-center gap-3 pt-2">
          <ActionButton
            type="button"
            variant="outline"
            size="sm"
            onClick={handleTest}
            actionState={actionState}
            targetState="testing"
            icon={Link2}
            activeLabel={t("settings.githubSync.testing")}
            idleLabel={t("settings.githubSync.test")}
          />
          <ActionButton
            type="button"
            variant="outline"
            size="sm"
            onClick={handleSave}
            actionState={actionState}
            targetState="saving"
            icon={Save}
            activeLabel={t("settings.githubSync.saving")}
            idleLabel={t("settings.githubSync.save")}
          />

          {/* Save status indicator */}
          {dirty && (
            <span className="inline-flex items-center gap-1.5 text-xs text-amber-500 dark:text-amber-400 animate-in fade-in duration-200">
              <span className="h-1.5 w-1.5 rounded-full bg-amber-500 dark:bg-amber-400" />
              {t("settings.githubSync.unsaved")}
            </span>
          )}
          {!dirty && justSaved && (
            <span className="inline-flex items-center gap-1.5 text-xs text-emerald-600 dark:text-emerald-400 animate-in fade-in duration-200">
              <Check className="h-3 w-3" />
              {t("settings.githubSync.saved")}
            </span>
          )}
        </div>

        {/* Sync buttons */}
        <div className="flex flex-wrap items-center gap-3 border-t border-border pt-4">
          <ActionButton
            type="button"
            size="sm"
            onClick={handleUploadClick}
            disabled={!hasSavedConfig}
            actionState={actionState}
            targetState="uploading"
            alsoActiveFor={["fetching_remote"]}
            icon={UploadCloud}
            activeLabel={
              actionState === "fetching_remote"
                ? t("settings.githubSync.fetchingRemote")
                : t("settings.githubSync.uploading")
            }
            idleLabel={t("settings.githubSync.upload")}
          />
          <ActionButton
            type="button"
            variant="secondary"
            size="sm"
            onClick={handleDownloadClick}
            disabled={!hasSavedConfig}
            actionState={actionState}
            targetState="downloading"
            alsoActiveFor={["fetching_remote"]}
            icon={DownloadCloud}
            activeLabel={
              actionState === "fetching_remote"
                ? t("settings.githubSync.fetchingRemote")
                : t("settings.githubSync.downloading")
            }
            idleLabel={t("settings.githubSync.download")}
          />
        </div>
        {!hasSavedConfig && (
          <p className="text-xs text-muted-foreground">
            {t("settings.githubSync.saveBeforeSync")}
          </p>
        )}
      </div>

      {/* ─── Upload confirmation dialog ──────────────────── */}
      <Dialog
        open={dialogType === "upload"}
        onOpenChange={(open) => {
          if (!open) closeDialog();
        }}
      >
        <DialogContent className="max-w-sm" zIndex="alert">
          <DialogHeader className="space-y-3 border-b-0 bg-transparent pb-0">
            <DialogTitle className="flex items-center gap-2 text-lg font-semibold">
              <AlertTriangle className="h-5 w-5 text-destructive" />
              {t("settings.githubSync.confirmUpload.title")}
            </DialogTitle>
            <DialogDescription asChild>
              <div className="space-y-3 text-sm leading-relaxed">
                <p>{t("settings.githubSync.confirmUpload.content")}</p>
                <ul className="list-disc pl-5 space-y-1 text-muted-foreground">
                  <li>{t("settings.githubSync.confirmUpload.dbItem")}</li>
                  <li>{t("settings.githubSync.confirmUpload.skillsItem")}</li>
                </ul>
                <p className="text-muted-foreground">
                  {t("settings.githubSync.confirmUpload.targetPath")}
                  {": "}
                  <code className="ml-1 text-xs bg-muted px-1.5 py-0.5 rounded">
                    {form.repo.trim()}/{form.remoteRoot.trim() || "cc-switch-sync"}/v2/
                    {form.profile.trim() || "default"}
                  </code>
                </p>
                {remoteInfo && (
                  <div className="rounded-lg border border-border bg-muted/50 p-3 space-y-2">
                    <p className="text-xs font-medium text-foreground">
                      {t("settings.githubSync.confirmUpload.existingData")}
                    </p>
                    <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1.5 text-xs text-muted-foreground">
                      <dt className="font-medium text-foreground">
                        {t("settings.githubSync.confirmUpload.deviceName")}
                      </dt>
                      <dd>
                        <code className="bg-muted px-1.5 py-0.5 rounded">
                          {remoteInfo.deviceName}
                        </code>
                      </dd>
                      <dt className="font-medium text-foreground">
                        {t("settings.githubSync.confirmUpload.createdAt")}
                      </dt>
                      <dd>{formatDate(remoteInfo.createdAt)}</dd>
                    </dl>
                  </div>
                )}
                {remoteInfo && (
                  <p className="text-destructive font-medium">
                    {t("settings.githubSync.confirmUpload.warning")}
                  </p>
                )}
              </div>
            </DialogDescription>
          </DialogHeader>
          <DialogFooter className="flex gap-2 border-t-0 bg-transparent pt-2 sm:justify-end">
            <Button variant="outline" onClick={closeDialog}>
              {t("common.cancel")}
            </Button>
            <Button variant="destructive" onClick={handleUploadConfirm}>
              {t("settings.githubSync.confirmUpload.confirm")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* ─── Download confirmation dialog ────────────────── */}
      <Dialog
        open={dialogType === "download"}
        onOpenChange={(open) => {
          if (!open) closeDialog();
        }}
      >
        <DialogContent className="max-w-sm" zIndex="alert">
          <DialogHeader className="space-y-3 border-b-0 bg-transparent pb-0">
            <DialogTitle className="flex items-center gap-2 text-lg font-semibold">
              <AlertTriangle className="h-5 w-5 text-destructive" />
              {t("settings.githubSync.confirmDownload.title")}
            </DialogTitle>
            <DialogDescription asChild>
              <div className="space-y-3 text-sm leading-relaxed">
                {remoteInfo && (
                  <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1.5 text-muted-foreground">
                    <dt className="font-medium text-foreground">
                      {t("settings.githubSync.confirmDownload.deviceName")}
                    </dt>
                    <dd>
                      <code className="text-xs bg-muted px-1.5 py-0.5 rounded">
                        {remoteInfo.deviceName}
                      </code>
                    </dd>
                    <dt className="font-medium text-foreground">
                      {t("settings.githubSync.confirmDownload.createdAt")}
                    </dt>
                    <dd>{formatDate(remoteInfo.createdAt)}</dd>
                    <dt className="font-medium text-foreground">
                      {t("settings.githubSync.confirmDownload.artifacts")}
                    </dt>
                    <dd>{remoteInfo.artifacts.join(", ")}</dd>
                  </dl>
                )}
                <p className="text-destructive font-medium">
                  {t("settings.githubSync.confirmDownload.warning")}
                </p>
              </div>
            </DialogDescription>
          </DialogHeader>
          <DialogFooter className="flex gap-2 border-t-0 bg-transparent pt-2 sm:justify-end">
            <Button variant="outline" onClick={closeDialog}>
              {t("common.cancel")}
            </Button>
            <Button variant="destructive" onClick={handleDownloadConfirm}>
              {t("settings.githubSync.confirmDownload.confirm")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </section>
  );
}
