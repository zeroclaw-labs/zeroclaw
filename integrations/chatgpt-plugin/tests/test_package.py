"""Standalone archive and manifest/component boundary checks; no account calls."""
import json
import importlib.util
from pathlib import Path
import tempfile
import unittest
import xml.etree.ElementTree as ET
import zipfile

PACKAGE = Path(__file__).resolve().parents[1]
COMPONENTS = (
    "plugin.json",
    ".codex-plugin/plugin.json",
    ".agents/plugins/marketplace.json",
    "skills/onboard/SKILL.md",
    "assets/icon.svg",
    "README.md",
)
SPEC = importlib.util.spec_from_file_location("chatgpt_package_builder", PACKAGE / "build_package.py")
BUILDER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BUILDER)


class PackageBoundaryTest(unittest.TestCase):
    def test_extracted_archive_discovers_skill_icons_and_local_catalog(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = root / "zeroclaw-chatgpt.zip"
            BUILDER.build(archive)
            extracted = root / "isolated"
            with zipfile.ZipFile(archive) as bundle:
                self.assertEqual(set(bundle.namelist()), set(COMPONENTS))
                bundle.extractall(extracted)
            portable = json.loads((extracted / "plugin.json").read_text())
            overlay = json.loads((extracted / ".codex-plugin/plugin.json").read_text())
            for field in ("name", "version", "description", "author", "repository"):
                self.assertEqual(portable[field], overlay[field])
            extension = portable["extensions"]["com.openai"]
            interface = extension["interface"]
            self.assertEqual(interface, overlay["interface"])
            self.assertLessEqual(len(interface["displayName"]), 30)
            self.assertLessEqual(len(interface["shortDescription"]), 30)
            self.assertIsInstance(interface["capabilities"], list)
            for key in ("composerIcon", "logo"):
                icon = extracted / interface[key][2:]
                self.assertTrue(interface[key].startswith("./"))
                self.assertTrue(icon.is_file())
                viewbox = ET.parse(icon).getroot().attrib["viewBox"].split()
                self.assertEqual(viewbox[2], viewbox[3])
                self.assertGreaterEqual(float(viewbox[2]), 48)
            skill = extracted / extension["onboardingSkill"][2:]
            self.assertTrue(skill.is_file())
            self.assertEqual(skill.parent.parent, extracted / "skills")
            self.assertEqual(extension["onboardingSkill"], overlay["extensions"]["com.openai"]["onboardingSkill"])
            self.assertEqual(overlay["skills"], "./skills/")
            catalog = json.loads((extracted / ".agents/plugins/marketplace.json").read_text())
            entry = catalog["plugins"][0]
            self.assertEqual(entry["name"], portable["name"])
            self.assertEqual(entry["source"], {"source": "local", "path": "./"})
            self.assertEqual(entry["policy"]["installation"], "AVAILABLE")
            self.assertTrue((extracted / entry["source"]["path"] / "plugin.json").is_file())
            for component in COMPONENTS:
                if component == ".codex-plugin/plugin.json":
                    self.assertEqual(overlay, json.loads((PACKAGE / component).read_text()))
                else:
                    self.assertEqual((extracted / component).read_bytes(), (PACKAGE / component).read_bytes())

    def test_archive_is_reproducible_and_overlay_is_generated_from_portable_manifest(self):
        with tempfile.TemporaryDirectory() as temporary:
            first = BUILDER.build(Path(temporary) / "first.zip")
            second = BUILDER.build(Path(temporary) / "second.zip")
            self.assertEqual(first.read_bytes(), second.read_bytes())
        self.assertEqual(BUILDER.compatibility_manifest(), json.loads((PACKAGE / ".codex-plugin/plugin.json").read_text()))

    def test_skill_only_distribution_has_no_external_server_or_autoexec(self):
        portable = json.loads((PACKAGE / "plugin.json").read_text())
        overlay = json.loads((PACKAGE / ".codex-plugin/plugin.json").read_text())
        for value in (portable, portable["extensions"]["com.openai"], overlay):
            self.assertFalse(set(value) & {"mcpServers", "apps", "hooks"})
        for component in ("mcp.json", ".mcp.json", ".app.json", "hooks"):
            self.assertFalse((PACKAGE / component).exists())


if __name__ == "__main__":
    unittest.main()
