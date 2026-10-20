import grass from "./assets/game-icons/grass.png";
import command from "./assets/game-icons/command.png";
import lamp from "./assets/game-icons/redstone-lamp.png";
import type { MessageKey } from "./i18n";
/** Only these bundled Minecraft item images are available. Item IDs
 * never select an unrelated launcher/loader icon or request arbitrary URLs. */
export const achievementIcons = {
  "minecraft:grass_block": grass,
  "minecraft:command_block": command,
  "minecraft:redstone_lamp": lamp,
} as const;
export type AchievementInput = {
  itemId: string;
  name: string;
  line1: string;
  line2: string;
};
export function achievementItem(input: string) {
  return input.includes(":") ? input.trim() : `minecraft:${input.trim()}`;
}
export function achievementError(input: AchievementInput): MessageKey | null {
  if (!(achievementItem(input.itemId) in achievementIcons))
    return "images.itemUnsupported";
  if (!input.name.trim() || !input.line1.trim()) return "images.textRequired";
  if (
    [input.name, input.line1, input.line2].some((value) =>
      /[^\x20-\x7e]/.test(value),
    )
  )
    return "images.englishOnly";
  return null;
}
/** A local achievement badge, rendered from actual input and a bundled item image.
 * The reference screenshot specifies the form, not exact generated pixels. */
export async function renderAchievement(
  input: AchievementInput,
): Promise<string> {
  const invalid = achievementError(input);
  if (invalid) throw new Error(invalid);
  const source =
    achievementIcons[
      achievementItem(input.itemId) as keyof typeof achievementIcons
    ];
  const image = new Image();
  await new Promise<void>((resolve, reject) => {
    image.onload = () => resolve();
    image.onerror = () => reject(new Error("images.assetUnavailable"));
    image.src = source;
  });
  const canvas = document.createElement("canvas"),
    context = canvas.getContext("2d");
  if (!context) throw new Error("images.canvasUnavailable");
  context.font = "14px monospace";
  const lines = [
    input.name,
    input.line1,
    ...(input.line2 ? [input.line2] : []),
  ];
  const width = Math.max(
    320,
    Math.ceil(
      Math.max(...lines.map((line) => context.measureText(line).width)),
    ) + 78,
  );
  if (width > 1024) throw new Error("images.textTooLong");
  canvas.width = width;
  canvas.height = input.line2 ? 82 : 64;
  context.imageSmoothingEnabled = false;
  context.fillStyle = "#070707";
  context.fillRect(0, 0, width, canvas.height);
  context.fillStyle = "#555555";
  context.fillRect(2, 2, width - 4, canvas.height - 4);
  context.fillStyle = "#202020";
  context.fillRect(5, 5, width - 10, canvas.height - 10);
  context.drawImage(image, 14, Math.floor((canvas.height - 32) / 2), 32, 32);
  context.font = "14px monospace";
  context.textBaseline = "top";
  context.fillStyle = "#ffff55";
  context.fillText(input.name, 58, 12);
  context.fillStyle = "#ffffff";
  context.fillText(input.line1, 58, 32);
  if (input.line2) context.fillText(input.line2, 58, 52);
  const encoded = canvas.toDataURL("image/png");
  if (!encoded.startsWith("data:image/png;base64,"))
    throw new Error("images.canvasUnavailable");
  return encoded;
}
