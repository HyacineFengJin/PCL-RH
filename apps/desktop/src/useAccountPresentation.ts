import { useEffect, useRef, useState } from "react";
import type { Api } from "./types";
import {
  appearanceKey,
  type AccountAppearance,
  type AuthState,
  type DeviceAssist,
} from "./accountTypes";

export function useAccountPresentation(
  api: Api,
  native: boolean,
  auth: AuthState,
) {
  const [images, setImages] = useState<Record<string, AccountAppearance>>({});
  const [assist, setAssist] = useState<{
    code: string;
    result: DeviceAssist;
  } | null>(null);
  const latest = useRef(auth);
  latest.current = auth;
  const completed = useRef(new Map<string, string>());
  const pending = useRef(new Map<string, { token: symbol; key: string }>());
  const attemptedCode = useRef<string | null>(null);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  async function refresh(id: string, force = true) {
    const profile = latest.current.accounts.find(
      (a) => a.profile.id === id,
    )?.profile;
    if (!native || !profile) return;
    const key = appearanceKey(profile);
    if (pending.current.get(id)?.key === key) return;
    const token = Symbol();
    pending.current.set(id, { token, key });
    try {
      const result = await api<AccountAppearance>("auth_appearance", {
        id,
        refresh: force,
      });
      const current = latest.current.accounts.find(
        (a) => a.profile.id === id,
      )?.profile;
      if (
        !mounted.current ||
        pending.current.get(id)?.token !== token ||
        !current ||
        appearanceKey(current) !== key
      )
        return;
      completed.current.set(id, key);
      setImages((old) => ({ ...old, [id]: result }));
    } catch {
      // Appearance errors cannot erase or downgrade the selected login.
    } finally {
      if (pending.current.get(id)?.token === token) pending.current.delete(id);
    }
  }
  const profilesKey = JSON.stringify(
    auth.accounts.map((a) => appearanceKey(a.profile)),
  );
  useEffect(() => {
    const ids = new Set(auth.accounts.map((a) => a.profile.id));
    setImages((old) =>
      Object.fromEntries(
        Object.entries(old).filter(([id]) => {
          const profile = auth.accounts.find(
            (a) => a.profile.id === id,
          )?.profile;
          return (
            profile && completed.current.get(id) === appearanceKey(profile)
          );
        }),
      ),
    );
    for (const id of completed.current.keys())
      if (!ids.has(id)) completed.current.delete(id);
    for (const id of pending.current.keys())
      if (!ids.has(id)) pending.current.delete(id);
    for (const account of auth.accounts) {
      const { id } = account.profile;
      if (
        completed.current.get(id) !== appearanceKey(account.profile) &&
        pending.current.get(id)?.key !== appearanceKey(account.profile)
      )
        void refresh(id, false);
    }
  }, [profilesKey, native]);
  const code = auth.challenge?.user_code || null;
  useEffect(() => {
    if (!code) {
      attemptedCode.current = null;
      setAssist(null);
      return;
    }
    if (!native || attemptedCode.current === code) return;
    attemptedCode.current = code;
    void api<DeviceAssist>("auth_device_assist", { code })
      .then((result) => {
        if (
          mounted.current &&
          latest.current.challenge?.user_code === code &&
          result.attempted
        )
          setAssist({ code, result });
      })
      .catch(() => {});
  }, [code, native]);
  return {
    images,
    refresh,
    assist: assist?.code === code ? assist.result : null,
  };
}
