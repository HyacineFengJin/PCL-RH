import type { AuthState, ProfileTextures } from "./accountTypes";

export type ProcessStatus = {
  stage: string;
  message: string;
  version: string | null;
  pid: number | null;
  exit_code: number | null;
  root_id?: string | null;
  root_path?: string | null;
};

export function sameProcessStatus(a: ProcessStatus, b: ProcessStatus) {
  return (
    a.stage === b.stage &&
    a.message === b.message &&
    a.version === b.version &&
    a.pid === b.pid &&
    a.exit_code === b.exit_code &&
    a.root_id === b.root_id &&
    a.root_path === b.root_path
  );
}
function sameTexture(
  a: ProfileTextures["skin"] | undefined,
  b: ProfileTextures["skin"] | undefined,
) {
  return a === b || (!!a && !!b && a.url === b.url && a.name === b.name);
}
function sameTextures(
  a: ProfileTextures | undefined,
  b: ProfileTextures | undefined,
) {
  return (
    a === b ||
    (!!a && !!b && sameTexture(a.skin, b.skin) && sameTexture(a.cape, b.cape))
  );
}
export function sameAuthState(a: AuthState, b: AuthState) {
  return (
    a.client_id === b.client_id &&
    a.selected === b.selected &&
    a.stage === b.stage &&
    a.message === b.message &&
    a.warning === b.warning &&
    a.saving === b.saving &&
    (a.challenge === b.challenge ||
      (!!a.challenge &&
        !!b.challenge &&
        a.challenge.user_code === b.challenge.user_code &&
        a.challenge.verification_uri === b.challenge.verification_uri &&
        a.challenge.expires_in === b.challenge.expires_in)) &&
    (a.saveable === b.saveable ||
      (!!a.saveable &&
        !!b.saveable &&
        a.saveable.length === b.saveable.length &&
        a.saveable.every((id, i) => id === b.saveable![i]))) &&
    a.accounts.length === b.accounts.length &&
    a.accounts.every((account, i) => {
      const other = b.accounts[i],
        p = account.profile,
        q = other.profile;
      return (
        account.remembered === other.remembered &&
        p.id === q.id &&
        p.uuid === q.uuid &&
        p.name === q.name &&
        p.expires_at === q.expires_at &&
        sameTextures(p.textures, q.textures)
      );
    })
  );
}

export type StatusReadSlot = { current: Promise<unknown> | null };

/** One physical read per owner. Wakes during a read demand a fresh successor;
 * superseded replies cannot overwrite the result of a user operation. Timers
 * are armed after settlement, so a slow native call never grows a work queue. */
export function createStatusPoller<T>({
  read,
  apply,
  interval,
  schedule,
  cancel,
  slot = { current: null },
}: {
  read(): Promise<T>;
  apply(value: T): void;
  interval(): number;
  schedule(callback: () => void, delay: number): number;
  cancel(timer: number): void;
  slot?: StatusReadSlot;
}) {
  let stopped = false,
    reading = false,
    requested = 0;
  let timer: number | undefined;
  const waiters: {
    ticket: number;
    resolve(): void;
    reject(error: unknown): void;
  }[] = [];
  const clear = () => {
    if (timer !== undefined) cancel(timer);
    timer = undefined;
  };
  const reschedule = () => {
    clear();
    if (!stopped && !reading)
      timer = schedule(() => {
        timer = undefined;
        wake();
      }, interval());
  };
  async function run() {
    if (stopped || reading) return;
    clear();
    reading = true;
    const ticket = requested;
    let failed = false,
      failure: unknown;
    try {
      // A closing/reopened effect still waits for the old physical IPC. The
      // retired owner consumes that reply but cannot update its replacement.
      if (slot.current) await slot.current.catch(() => {});
      if (stopped || ticket !== requested) return;
      const pending = read();
      slot.current = pending;
      let value: T;
      try {
        value = await pending;
      } finally {
        if (slot.current === pending) slot.current = null;
      }
      if (!stopped && ticket === requested) apply(value);
    } catch (error) {
      failed = true;
      failure = error;
    } finally {
      reading = false;
      for (let i = waiters.length - 1; i >= 0; i--)
        if (waiters[i].ticket <= ticket) {
          const waiter = waiters.splice(i, 1)[0];
          if (failed && !stopped) waiter.reject(failure);
          else waiter.resolve();
        }
      if (!stopped && requested > ticket) void run();
      else reschedule();
    }
  }
  function wake() {
    if (stopped) return;
    requested++;
    void run();
  }
  return {
    wake,
    reschedule,
    refresh: () =>
      new Promise<void>((resolve, reject) => {
        if (stopped) {
          resolve();
          return;
        }
        waiters.push({ ticket: ++requested, resolve, reject });
        void run();
      }),
    stop() {
      stopped = true;
      clear();
      for (const waiter of waiters.splice(0)) waiter.resolve();
    },
  };
}
