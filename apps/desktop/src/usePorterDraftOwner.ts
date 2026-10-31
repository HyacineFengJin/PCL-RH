import { useEffect, useRef } from "react";
import type { ExperimentalDraft } from "./ExperimentalTools";
import { createPorterSourceOwner } from "./porterSourceOwner";

/** Page changes retain this draft. Only its actual parent lifetime releases it. */
export function usePorterDraftOwner() {
  const drafts = useRef<
    Partial<Record<"maker" | "porter" | "extensions", ExperimentalDraft>>
  >({});
  const sourceOwner = useRef(createPorterSourceOwner()).current;
  const lifetime = useRef(0);
  useEffect(() => {
    const generation = ++lifetime.current;
    return () => {
      // A synchronous effect replay still owns the same draft and receipt.
      queueMicrotask(() => {
        if (lifetime.current === generation) void sourceOwner.dispose();
      });
    };
  }, [sourceOwner]);
  return { drafts, sourceOwner };
}
