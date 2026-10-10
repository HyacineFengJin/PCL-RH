import { currentLocale } from "../i18n";
export const label = (...pair: readonly string[]) =>
  currentLocale().language === "en-US" ? pair[1] : pair[0];
export const viewNames = {
  legacy: ["旧版项目", "Previous projects"],
  chat: ["AI 对话", "AI conversation"],
  threads: ["项目对话", "Project conversations"],
  graph: ["内容图谱", "Content graph"],
  files: ["文件图谱", "Project files"],
  source: ["源码编辑", "Source editor"],
  targets: ["构建与测试", "Build and test"],
  changes: ["改动审阅", "Changes and review"],
  logs: ["任务与输出", "Tasks and output"],
  porting: ["版本迁移", "Version migration"],
} as const;
