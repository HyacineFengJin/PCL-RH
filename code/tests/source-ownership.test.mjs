import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { reconcileNativeOwnership } from "../source-ownership.mjs";
import { snapshotProtection } from "../../experimental/runtime/vendor/maker/protection.mjs";
const maker = new URL(
  "../../experimental/runtime/vendor/maker/",
  import.meta.url,
).pathname;
const digest = (data) => createHash("sha256").update(data).digest("hex");
const py = (script, input) =>
  execFileSync("python3", ["-c", script], {
    cwd: maker,
    input,
    maxBuffer: 1024 * 1024,
  }).toString();
const generated = () =>
  Object.fromEntries(
    Object.entries(
      JSON.parse(
        py(`
import mod_creator as m, source_editor as s, json, base64
spec = {"schema_version":1,"target":"fabric-1.21.1","mod_id":"fixture_mod","name":"Fixture","description":"Fixture","items":[{"id":"fixture_item","names":{"zh_cn":"物品","en_us":"Item"},"color":"#8160b0","max_count":64,"recipe":{"ingredients":["minecraft:paper"],"count":1}}]}
files = m.render(spec)
files = s.owned_snapshot(files, {p:"template" for p in files}, files)
print(json.dumps({p:base64.b64encode(b).decode() for p,b in files.items()}))
`),
      ),
    ).map(([path, data]) => [path, Buffer.from(data, "base64")]),
  );

test("manual Java edits become protected in the job copy and pass the existing full Maker audit", () => {
  const files = generated();
  const java = Object.keys(files).find((p) => p.endsWith(".java"));
  const metadataBefore = Buffer.from(files["creator-ownership.json"]);
  files[java] = Buffer.concat([
    files[java],
    Buffer.from("\n// handwritten implementation\n"),
  ]);
  const extra = "src/main/java/fixture/Handwritten.java";
  files[extra] = Buffer.from("package fixture; class Handwritten {}\n");
  assert.deepEqual(reconcileNativeOwnership(files), [extra, java].sort());
  assert.deepEqual(snapshotProtection({ files }), [extra, java].sort());
  const records = JSON.parse(files["creator-ownership.json"]).files;
  assert.equal(records[java].sha256, digest(files[java]));
  assert.equal(JSON.parse(metadataBefore).files[java].owner, "template");
  const encoded = JSON.stringify(
    Object.fromEntries(
      Object.entries(files).map(([path, bytes]) => [
        path,
        bytes.toString("base64"),
      ]),
    ),
  );
  assert.equal(
    py(
      `
import source_editor as s, tempfile, json, base64, pathlib, sys
with tempfile.TemporaryDirectory(prefix="rh-ownership-fixture-") as tmp:
    root = pathlib.Path(tmp)
    for name, raw in json.load(sys.stdin).items():
        path = root / name; path.parent.mkdir(parents=True, exist_ok=True); path.write_bytes(base64.b64decode(raw))
    s.audit(root)
    print("checked")
`,
      encoded,
    ).trim(),
    "checked",
  );
});

test("control-file edits and missing receipt files do not silently reconcile", () => {
  for (const kind of ["build", "missing"]) {
    const files = generated();
    if (kind === "build") files["build.gradle"] = Buffer.from("modified build");
    else delete files[Object.keys(files).find((p) => p.endsWith(".java"))];
    assert.throws(
      () => reconcileNativeOwnership(files),
      /control file|inventory/,
    );
  }
});
