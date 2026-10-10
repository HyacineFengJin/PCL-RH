import { useState } from "react";
import { Plus, FolderOpen, ArrowUpRight, Search } from "lucide-react";
import rhMark from "../../../../assets/branding/rh-mark.svg";
import { useCode } from "./controller";
import { label } from "./locale";
import { Dialog } from "./Dialog";
export function Projects({ onArchive }: { onArchive: () => void }) {
  const c = useCode(),
    [creating, setCreating] = useState(false),
    [name, setName] = useState(""),
    [folder, setFolder] = useState(""),
    [query, setQuery] = useState("");
  async function create() {
    const snapshot = await c.write("project_create", { name, folder });
    c.choose(snapshot.projects.at(-1)!);
    setCreating(false);
  }
  return (
    <div className="rc-projects">
      <header>
        <div className="rc-project-brand">
          <img src={rhMark} alt="RH" />
          <span>Code</span>
        </div>
        <div className="rc-row">
          <button className="rc-button" onClick={onArchive}>
            {label("旧版项目", "Previous projects")}
          </button>
          <button
            className="rc-button purple"
            disabled={!c.native}
            onClick={() => setCreating(true)}
          >
            <Plus size={16} />
            {label("新建项目", "New project")}
          </button>
        </div>
      </header>
      <div className="rc-project-heading">
        <h1>{label("你的创作空间", "Your creative workspace")}</h1>
        <label className="rc-search">
          <Search size={16} />
          <input
            placeholder={label("搜索项目", "Search projects")}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </label>
      </div>
      <div className="rc-project-list">
        {c.snapshot?.projects
          .filter((p) =>
            (p.name + " " + p.folder)
              .toLowerCase()
              .includes(query.toLowerCase()),
          )
          .map((p) => (
            <button
              className="rc-project-card"
              key={p.id}
              onClick={() => c.choose(p)}
            >
              <div className="rc-project-art">
                <FolderOpen size={29} />
                <span>{p.targets.length}</span>
              </div>
              <div>
                <h2>{p.name}</h2>
                <p title={p.folder}>{p.folder}</p>
                <small>
                  {p.conversations.length} {label("个对话", "conversations")} ·{" "}
                  {p.targets.map((t) => t.minecraft).join(" / ")}
                </small>
              </div>
              <ArrowUpRight size={18} />
            </button>
          ))}
      </div>
      {!c.snapshot?.projects.length && (
        <div className="rc-project-empty">
          <div className="rc-zeta">ζ</div>
          <h2>{label("从一个文件夹开始", "Start with a folder")}</h2>
          <button
            className="rc-button purple"
            disabled={!c.native}
            onClick={() => setCreating(true)}
          >
            <Plus size={16} />
            {label("创建项目", "Create project")}
          </button>
        </div>
      )}
      {creating && (
        <Dialog
          title={label("新建项目", "New project")}
          onClose={() => setCreating(false)}
        >
          <form
            onSubmit={(e) => {
              e.preventDefault();
              void create().catch(() => {});
            }}
          >
            <label className="rc-field">
              {label("名称", "Name")}
              <input
                autoFocus
                maxLength={120}
                value={name}
                onChange={(e) => setName(e.target.value)}
              />
            </label>
            <label className="rc-field">
              {label("项目文件夹", "Project folder")}
              <div className="rc-row">
                <input
                  value={folder}
                  onChange={(e) => setFolder(e.target.value)}
                />
                <button
                  type="button"
                  className="rc-button"
                  onClick={() =>
                    void c
                      .api<string | null>("rh_code_choose", {})
                      .then((p) => {
                        if (p) setFolder(p);
                      })
                      .catch((e) => c.setError(String(e)))
                  }
                >
                  <FolderOpen size={17} />
                </button>
              </div>
            </label>
            <footer>
              <button
                className="rc-button purple"
                disabled={
                  !c.native || !!c.busy || !name.trim() || !folder.trim()
                }
              >
                <Plus size={16} />
                {label("创建", "Create")}
              </button>
            </footer>
          </form>
        </Dialog>
      )}
    </div>
  );
}
