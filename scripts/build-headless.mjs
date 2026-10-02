import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, rmSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const release = process.argv.slice(2).includes("--release");
const workspace = fileURLToPath(new URL("../", import.meta.url));
const result = spawnSync("cargo", [
    "build", "--manifest-path", fileURLToPath(new URL("../Cargo.toml", import.meta.url)),
    "-p", "dg-lab-link-core-server", "-p", "dg-lab-link-cli", "-p", "dg-lab-link-mcp",
    "-p", "dg-lab-link-builtin-plugins", "-p", "dg-lab-link-plugin-runtime",
    ...(release ? ["--release"] : []),
], { cwd: workspace, stdio: "inherit", windowsHide: true });
if (result.error) {
    console.error(result.error.message);
}
if (result.status !== 0) {
    process.exit(result.status ?? 1);
}
const metadata = spawnSync("cargo", ["metadata", "--no-deps", "--format-version", "1"], {
    cwd: workspace, encoding: "utf8", windowsHide: true,
});
if (metadata.status !== 0) {
    process.stderr.write(metadata.stderr ?? "Unable to locate Cargo artifacts\n");
    process.exit(metadata.status ?? 1);
}
const target = resolve(JSON.parse(metadata.stdout).target_directory);
const artifacts = join(target, release ? "release" : "debug");
const extension = process.platform === "win32" ? ".exe" : "";
const staging = resolve(target, `.plugin-pack-${process.pid}`);
const packages = join(artifacts, "plugins");
mkdirSync(packages, { recursive: true });
try {
    for (const kind of ["touch", "audio"]) {
        const payload = join(staging, kind);
        mkdirSync(payload, { recursive: true });
        copyFileSync(join(workspace, "crates", "builtin-plugins", "packages", kind, "plugin.json"), join(payload, "plugin.json"));
        copyFileSync(join(artifacts, `dg-lab-link-${kind}${extension}`), join(payload, `dg-lab-link-${kind}${extension}`));
        const packed = spawnSync(join(artifacts, `dg-lab-link-plugin-pack${extension}`), [
            payload, join(packages, `cn.dglab.link.${kind}.dglabplugin`),
        ], { cwd: workspace, stdio: "inherit", windowsHide: true });
        if (packed.status !== 0) {
            process.exitCode = packed.status ?? 1;
            break;
        }
    }
} finally {
    // Only remove this build's explicitly checked, task-owned staging directory.
    if (dirname(staging) === target && basename(staging) === `.plugin-pack-${process.pid}`) {
        rmSync(staging, { recursive: true, force: true });
    }
}
