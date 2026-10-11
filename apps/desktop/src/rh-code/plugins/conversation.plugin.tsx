import type { BundledPluginEntry } from "../kernel";
import { MessageSquare, MessagesSquare } from "lucide-react";
import { label } from "../locale";
import { Chat, Conversations } from "./conversations";
const entry: BundledPluginEntry = {
  id: "rh.conversation",
  create: (manifest) => ({
    manifest,
    views: [
      {
        id: "chat",
        owner: manifest.id,
        label: label("AI 对话", "AI conversation"),
        icon: MessageSquare,
        tone: "purple",
        render: Chat,
      },
      {
        id: "threads",
        owner: manifest.id,
        label: label("项目对话", "Project conversations"),
        icon: MessagesSquare,
        tone: "purple",
        render: Conversations,
      },
    ],
  }),
};
export default entry;
