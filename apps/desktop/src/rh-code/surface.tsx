import { createContext, useContext } from "react";

/** Hidden Code stays mounted to own drafts and running jobs. Community views
 * use visibility to retire requests, including a departure followed by return.
 * The default preserves standalone views outside the launcher-owned surface. */
export const CodeSurfaceVisibility = createContext(true);
export const useCodeSurfaceVisible = () => useContext(CodeSurfaceVisibility);
