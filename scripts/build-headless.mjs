import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const release = process.argv.slice(2).includes("--release");
const workspace = fileURLToPath(new URL("../", import.meta.url));
const result = spawnSync("cargo", [
    "build", "--manifest-path", fileURLToPath(new URL("../Cargo.toml", import.meta.url)),
    "-p", "dg-lab-link-core-server", "-p", "dg-lab-link-cli", ...(release ? ["--release"] : []),
], { cwd: workspace, stdio: "inherit", windowsHide: true });
if (result.error) {
    console.error(result.error.message);
}
process.exit(result.status ?? 1);
