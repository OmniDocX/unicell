"""Exercise the publication check against real, isolated Git indexes."""

import os
import io
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

from check_public_boundary import scan_index, scan_history, scan_content


class PublicBoundaryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="unippt-publication-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.git("init", "--quiet")

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.root), *args], check=True, capture_output=True)

    def put(self, name, content, stage=True):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        if stage:
            self.git("add", "-f", "--", name)
        return path

    def test_template_and_generic_markers_are_allowed(self):
        self.put(".env.example", b"UNIPPT_AI_KEY=\n")
        self.put("README.md", b"BEGIN PRIVATE KEY; ghp_; sk-proj-; glpat-; AKIA")
        self.assertEqual(scan_index(self.root), [])

    def test_force_added_private_paths_are_rejected_even_when_empty(self):
        names = [".env", "config/.env.local", ".env.production", "tls/cert.key",
                 "ops/account.dpapi.xml", ".unippt-mcp/token.json",
                 "deploy/UNIPPT_PRODUCTION_HANDOFF_20260912.md", "keys/id_ed25519"]
        for name in names:
            self.put(name, b"")
        self.assertEqual(len(scan_index(self.root)), len(names))

    def test_ignored_local_files_are_not_read(self):
        self.put(".gitignore", b".env.local\n")
        self.put(".env.local", b"local-only", stage=False)
        self.assertEqual(scan_index(self.root), [])

    def test_staged_secret_survives_safe_worktree_replacement(self):
        token = b"glpat-" + b"x" * 24
        path = self.put("config.json", token)
        path.write_bytes(b"safe unstaged change")
        hits = scan_index(self.root)
        self.assertEqual(hits, ["config.json: GitLab token"])
        self.assertNotIn(token.decode(), "\n".join(hits))

    def test_unstaged_secret_does_not_change_index_result(self):
        path = self.put("config.json", b"safe index")
        path.write_bytes(b"ghp_" + b"x" * 40)
        self.assertEqual(scan_index(self.root), [])

    def test_deleted_worktree_file_still_scans_index(self):
        path = self.put("settings", b"sk-proj-" + b"x" * 24)
        path.unlink()
        self.assertEqual(scan_index(self.root), ["settings: OpenAI project key"])

    def test_all_extensions_and_old_exemption_are_scanned(self):
        header = b"-----" + b"BEGIN " + b"PRIVATE KEY" + b"-----"
        for name in ["payload.bin", "config", "deploy/test-public-boundary.py"]:
            self.put(name, b"\x00" + header)
        self.assertEqual(sum('private key' in hit for hit in scan_index(self.root)), 3)

    def test_private_records_and_compressed_personal_metadata_are_rejected(self):
        self.put("ara/trace/session.yaml", b"routine record")
        out = io.BytesIO()
        with zipfile.ZipFile(out, 'w', zipfile.ZIP_DEFLATED) as archive:
            archive.writestr('notes', b'https://chatgpt.com/' + b'c/private-session')
            archive.writestr('docProps/core.xml', '<root><creator>Private author</creator></root>')
        self.put('fixtures/example.pptx', out.getvalue())
        hits = scan_index(self.root)
        self.assertTrue(any('private file' in hit for hit in hits))
        self.assertTrue(any('conversation URL' in hit for hit in hits))
        self.assertTrue(any('author metadata' in hit for hit in hits))
        self.assertNotIn('Private author', '\n'.join(hits))

    def test_archive_failures_are_not_silently_accepted(self):
        self.assertTrue(scan_content('bad.zip', b'PK\x03\x04broken'))

    def test_history_detects_deleted_private_files_and_personal_commit_email(self):
        self.put('ara/notes.md', b'private record')
        self.git('-c', 'user.name=Test', '-c', 'user.email=person@'+'qq.com', 'commit', '-qm', 'fixture')
        self.git('rm', '-q', 'ara/notes.md')
        self.put('README.md', b'public source')
        self.git('-c', 'user.name=Test', '-c', 'user.email=cc@omnidoc.top', 'commit', '-qm', 'public tree')
        self.assertEqual(scan_index(self.root), [])
        hits = scan_history(self.root)
        self.assertTrue(any('mailbox' in hit for hit in hits))
        self.assertTrue(any('private file' in hit for hit in hits))

    def test_vendor_attribution_and_business_contact_are_allowed(self):
        self.assertEqual(scan_content('README.md', b'cc@omnidoc.top'), [])
        out = io.BytesIO()
        with zipfile.ZipFile(out, 'w') as archive:
            archive.writestr('docProps/core.xml', '<root><creator>Upstream author</creator></root>')
        self.assertEqual(scan_content('vendor/library/default.pptx', out.getvalue()), [])

    def test_all_supported_token_signatures(self):
        tokens = [b"AKIA" + b"A" * 16, b"ASIA" + b"A" * 16,
                  b"ghp_" + b"x" * 40, b"github_pat_" + b"x" * 60,
                  b"sk-proj-" + b"x" * 24, b"glpat-" + b"x" * 24]
        for i, token in enumerate(tokens):
            self.put(f"file{i}.json", token)
        self.assertEqual(len(scan_index(self.root)), len(tokens))

    def test_cli_works_from_subdirectory_and_does_not_echo_values(self):
        script = Path(__file__).with_name("check_public_boundary.py").resolve()
        token = b"glpat-" + b"z" * 24
        self.put("nested/config", token)
        result = subprocess.run([sys.executable, str(script)], cwd=self.root / "nested", capture_output=True)
        self.assertEqual(result.returncode, 1)
        self.assertNotIn(token, result.stdout + result.stderr)

    def test_missing_repository_fails_closed(self):
        script = Path(__file__).with_name("check_public_boundary.py").resolve()
        result = subprocess.run([sys.executable, str(script), "--repo", str(self.root / "missing")], capture_output=True)
        self.assertEqual(result.returncode, 2)

    def test_container_checkout_requires_explicit_trust(self):
        script = Path(__file__).with_name("check_public_boundary.py").resolve()
        self.put("README.md", b"public source")
        # Reproduce a container user differing from the checkout owner without
        # changing filesystem ownership or the developer's Git configuration.
        env = dict(os.environ, GIT_TEST_ASSUME_DIFFERENT_OWNER="1",
                   GIT_CONFIG_COUNT="1", GIT_CONFIG_KEY_0="safe.directory",
                   GIT_CONFIG_VALUE_0="")
        cmd = [sys.executable, str(script), "--repo", str(self.root)]
        result = subprocess.run(cmd, env=env, capture_output=True)
        self.assertEqual(result.returncode, 2)
        env.update(GIT_CONFIG_COUNT="2", GIT_CONFIG_KEY_1="safe.directory",
                   GIT_CONFIG_VALUE_1=self.root.resolve().as_posix())
        result = subprocess.run(cmd, env=env, capture_output=True)
        self.assertEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
