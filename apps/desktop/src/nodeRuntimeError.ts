/** Only the native host's fixed prerequisite tags permit dependency guidance.
 * Store, permission, identity, transport and cleanup failures stay ordinary
 * service errors even when their messages mention Node.js. */
export function nodeRuntimeError(
  error: unknown,
): "missing" | "unsupported" | null {
  const raw =
    error instanceof Error
      ? error.message
      : typeof error === "string"
        ? error
        : "";
  // Controllers retain String(error), including the standard Error wrapper.
  // Strip exactly that one wrapper; never search arbitrary service text.
  const message = raw.startsWith("Error: ") ? raw.slice(7) : raw;
  if (message.startsWith("PCL_NODE_MISSING:")) return "missing";
  if (message.startsWith("PCL_NODE_UNSUPPORTED:")) return "unsupported";
  return null;
}
