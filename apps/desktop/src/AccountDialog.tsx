import { useState } from "react";
import {
  ArrowLeft,
  Check,
  Copy,
  ExternalLink,
  LoaderCircle,
  RefreshCw,
  Save,
  Shirt,
  UserRound,
  X,
} from "lucide-react";
import { t } from "./i18n";
import type {
  AccountAppearance,
  AuthState,
  DeviceAssist,
} from "./accountTypes";
import defaultSkin from "./assets/game-icons/steve.png";
import "./account-dialog.css";

export function AccountAvatar({
  image,
  className = "",
}: {
  image?: string | null;
  className?: string;
}) {
  return image ? (
    <img className={`ce-account-avatar ${className}`} src={image} alt="" />
  ) : (
    <span
      className={`ce-account-avatar ce-account-avatar-default ${className}`}
      style={{ backgroundImage: `url(${defaultSkin}),url(${defaultSkin})` }}
      aria-hidden="true"
    />
  );
}
type Props = {
  mode: "offline" | "premium";
  player: string;
  auth: AuthState;
  native: boolean;
  busy: boolean;
  working: boolean;
  error: string;
  images: Record<string, AccountAppearance>;
  assist: DeviceAssist | null;
  remember: boolean;
  onRemember: (value: boolean) => void;
  onAction: (command: string, args?: Record<string, unknown>) => Promise<void>;
  onOfflineSave: (name: string) => Promise<void>;
  onAppearance: (id: string) => void;
  onBack: () => void;
  onClose: () => void;
};
export function AccountDialog(p: Props) {
  const [name, setName] = useState(p.player);
  const [nameError, setNameError] = useState("");
  const signing = ["preparing", "waiting", "saving"].includes(p.auth.stage);
  const disabled = p.working || p.busy || signing;
  const problem =
    p.error ||
    p.auth.warning ||
    (p.auth.stage === "error" ? p.auth.message : "");
  return (
    <div
      className="modal-shade"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) p.onClose();
      }}
    >
      <section
        className="ce-account-modal"
        role="dialog"
        aria-modal="true"
        aria-label={t(
          p.mode === "offline"
            ? "accounts.offlineTitle"
            : "accounts.onlineTitle",
        )}
      >
        <header className="ce-account-heading">
          <button
            className="icon-button"
            onClick={p.onBack}
            disabled={signing || p.working}
            aria-label={t("accounts.back")}
          >
            <ArrowLeft size={18} />
          </button>
          <h2>
            {t(
              p.mode === "offline"
                ? "accounts.offlineTitle"
                : "accounts.onlineTitle",
            )}
          </h2>
          <button
            className="icon-button"
            onClick={p.onClose}
            aria-label={t("main.closeDialog")}
          >
            <X size={20} />
          </button>
        </header>
        {p.mode === "offline" ? (
          <form
            className="ce-account-body"
            noValidate
            onSubmit={(e) => {
              e.preventDefault();
              if (!/^[A-Za-z0-9_]{3,16}$/.test(name.trim())) {
                setNameError(t("accounts.nameInvalid"));
                return;
              }
              setNameError("");
              void p.onOfflineSave(name.trim());
            }}
          >
            <label className="ce-row">
              <span>{t("accounts.offlineName")}</span>
              <input
                className="ce-field"
                value={name}
                maxLength={16}
                autoComplete="off"
                disabled={p.working || p.busy}
                onChange={(e) => setName(e.target.value)}
              />
            </label>
            {(nameError || p.error || p.busy) && (
              <p className="ce-account-error" role="alert">
                {nameError || p.error || t("accounts.busy")}
              </p>
            )}
            <footer className="ce-account-footer">
              <button
                className="ce-button primary"
                type="submit"
                disabled={!p.native || p.working || p.busy}
              >
                <Save size={16} />
                {p.working ? t("main.saving") : t("accounts.save")}
              </button>
            </footer>
          </form>
        ) : (
          <div className="ce-account-body">
            {p.auth.accounts.length > 0 && (
              <div className="ce-account-saved-list">
                {p.auth.accounts.map((a) => (
                  <div
                    className={
                      "ce-account-saved-row " +
                      (a.profile.id === p.auth.selected ? "selected" : "")
                    }
                    key={a.profile.id}
                  >
                    <button
                      className="ce-account-select"
                      disabled={!p.native || disabled}
                      onClick={() =>
                        void p.onAction("auth_select", { id: a.profile.id })
                      }
                    >
                      <AccountAvatar
                        image={p.images[a.profile.id]?.avatar_png}
                      />
                      <span>
                        <strong>{a.profile.name}</strong>
                        <small>
                          {t(
                            a.remembered
                              ? "accounts.saved"
                              : "accounts.unsaved",
                          )}
                        </small>
                      </span>
                      {a.profile.id === p.auth.selected && <Check size={17} />}
                    </button>
                    {!a.remembered && (
                      <button
                        className="ce-button ce-account-save"
                        disabled={
                          !p.native ||
                          disabled ||
                          !p.auth.saveable?.includes(a.profile.id)
                        }
                        onClick={() =>
                          void p.onAction("auth_save_login", {
                            id: a.profile.id,
                          })
                        }
                      >
                        <Save size={15} />
                        {t("accounts.save")}
                      </button>
                    )}
                    <button
                      className="icon-button"
                      disabled={!p.native}
                      title={t("accounts.appearance")}
                      aria-label={
                        t("accounts.appearance") + " " + a.profile.name
                      }
                      onClick={() => p.onAppearance(a.profile.id)}
                    >
                      <Shirt size={17} />
                    </button>
                    <button
                      className="icon-button"
                      disabled={!p.native || disabled}
                      title={t("accounts.remove")}
                      aria-label={t("accounts.remove") + " " + a.profile.name}
                      onClick={() =>
                        void p.onAction("auth_remove", { id: a.profile.id })
                      }
                    >
                      <X size={17} />
                    </button>
                  </div>
                ))}
              </div>
            )}
            {p.auth.challenge ? (
              <div className="ce-account-code-card">
                <span>{t("accounts.deviceCodeShort")}</span>
                <strong className="device-code">
                  {p.auth.challenge.user_code}
                </strong>
                {p.assist?.copied && (
                  <small className="ce-account-assist">
                    <Check size={14} />
                    {t("accounts.codeCopied")}
                  </small>
                )}
                <div className="ce-account-actions">
                  <button
                    className="ce-button"
                    disabled={p.working}
                    onClick={() =>
                      void p.onAction("auth_copy_code", {
                        code: p.auth.challenge!.user_code,
                      })
                    }
                  >
                    <Copy size={15} />
                    {t("accounts.copyCode")}
                  </button>
                  <button
                    className="ce-button"
                    disabled={p.working}
                    onClick={() => void p.onAction("auth_open_browser")}
                  >
                    <ExternalLink size={15} />
                    {t("accounts.browserShort")}
                  </button>
                  <button
                    className="ce-button"
                    disabled={p.working}
                    onClick={() => void p.onAction("auth_cancel")}
                  >
                    {t("common.cancel")}
                  </button>
                </div>
              </div>
            ) : (
              <div className="ce-account-login-actions">
                <label className="remember-login">
                  <input
                    type="checkbox"
                    checked={p.remember}
                    disabled={disabled}
                    onChange={(e) => p.onRemember(e.target.checked)}
                  />
                  {t("accounts.remember")}
                </label>
                <button
                  className="ce-button primary ce-account-login"
                  disabled={!p.native || disabled || !p.auth.client_id}
                  onClick={() =>
                    void p.onAction("auth_start", { remember: p.remember })
                  }
                >
                  {signing ? (
                    <LoaderCircle size={18} className="spin" />
                  ) : (
                    <UserRound size={18} />
                  )}
                  {t("accounts.signIn")}
                </button>
              </div>
            )}
            {signing && p.auth.stage !== "waiting" && (
              <p className="ce-account-progress" role="status">
                <LoaderCircle size={15} className="spin" />
                {p.auth.message}
              </p>
            )}
            {(problem || p.busy) && (
              <p className="ce-account-error" role="alert">
                {problem || t("accounts.busy")}
              </p>
            )}
          </div>
        )}
      </section>
    </div>
  );
}
export function AccountAppearanceDialog(p: {
  name: string;
  image?: AccountAppearance;
  onRefresh: () => void;
  onClose: () => void;
}) {
  return (
    <div
      className="modal-shade"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) p.onClose();
      }}
    >
      <section
        className="ce-account-modal"
        role="dialog"
        aria-modal="true"
        aria-label={t("accounts.appearance")}
      >
        <header className="ce-account-heading">
          <Shirt size={20} />
          <h2>{p.name}</h2>
          <button
            className="icon-button"
            onClick={p.onClose}
            aria-label={t("main.closeDialog")}
          >
            <X size={20} />
          </button>
        </header>
        <div className="ce-account-body">
          <div className="ce-account-appearance-grid">
            <div className="ce-account-appearance-card">
              <h3>{t("accounts.avatar")}</h3>
              <AccountAvatar image={p.image?.avatar_png} className="large" />
            </div>
            <div className="ce-account-appearance-card">
              <h3>{t("accounts.cape")}</h3>
              {!p.image ? (
                <LoaderCircle size={24} className="spin" />
              ) : p.image.cape_png ? (
                <>
                  <img
                    className="ce-account-cape"
                    src={p.image.cape_png}
                    alt=""
                  />
                  <small>{p.image.cape_name}</small>
                </>
              ) : (
                <span className="muted">
                  {t(
                    p.image.has_cape
                      ? "accounts.capeUnavailable"
                      : "accounts.noCape",
                  )}
                </span>
              )}
            </div>
          </div>
          {p.image?.warning && (
            <p className="ce-account-error" role="status">
              {p.image.warning}
            </p>
          )}
          <footer className="ce-account-footer">
            <button className="ce-button" onClick={p.onRefresh}>
              <RefreshCw size={15} />
              {t("accounts.refreshAppearance")}
            </button>
            <button className="ce-button" onClick={p.onClose}>
              {t("main.closeDialog")}
            </button>
          </footer>
        </div>
      </section>
    </div>
  );
}
