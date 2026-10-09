export type ProfileTextures = {
  skin: { url: string; name: string } | null;
  cape: { url: string; name: string } | null;
};
export type AccountProfile = {
  id: string;
  uuid: string;
  name: string;
  expires_at: number;
  textures?: ProfileTextures;
};
export type AuthState = {
  client_id: string;
  selected: string | null;
  accounts: { profile: AccountProfile; remembered: boolean }[];
  stage: string;
  message: string;
  challenge: {
    user_code: string;
    verification_uri: string;
    expires_in: number;
  } | null;
  warning: string | null;
  saveable?: string[];
  saving?: boolean;
};
export type AccountAppearance = {
  avatar_png: string | null;
  skin_png: string | null;
  cape_png: string | null;
  cape_name: string;
  has_cape: boolean;
  warning: string | null;
};
export type DeviceAssist = {
  attempted: boolean;
  copied: boolean;
  opened: boolean;
};

// Expiry/selection changes do not refetch unchanged public textures. An account
// replacement or new texture invalidates the old row's asynchronous response.
export function appearanceKey(profile: AccountProfile): string {
  return JSON.stringify([profile.id, profile.textures || null]);
}
