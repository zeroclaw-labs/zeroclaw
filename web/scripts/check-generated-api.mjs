import { existsSync } from "node:fs";
import { resolve } from "node:path";

const generatedFiles = [
  "src/lib/api-generated.ts",
  "src/lib/api-descriptions.ts",
  "src/lib/api-enums.ts",
];
const missingFiles = generatedFiles.filter((file) => !existsSync(resolve(file)));

if (missingFiles.length > 0) {
  console.error(
    "Generated dashboard API files are missing. Run `cargo web check` from the repository root.",
  );
  for (const file of missingFiles) {
    console.error(`Missing: ${file}`);
  }
  process.exit(1);
}
