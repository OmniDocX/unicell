"""Guard publication boundaries and the static asset graph."""
from pathlib import Path
import re
import unittest
from check_public_boundary import private_path

ROOT=Path(__file__).resolve().parents[1]

class LocalEdition(unittest.TestCase):
    def test_hosted_modules_cannot_reenter_public_index(self):
        for name in ['cloud_storage.rs','collaboration_runtime.rs','shared_workbooks.rs','omnidoc_connect.rs','production_state.rs','ai_quota.rs']:
            self.assertTrue(private_path('server/src/'+name))
            self.assertFalse((ROOT/'server/src'/name).exists())
        self.assertTrue(private_path('server/target/debug/server.exe'))

    def test_all_editor_scripts_and_styles_exist_locally(self):
        html=(ROOT/'web/index.html').read_text(encoding='utf-8')
        urls=re.findall(r'(?:src|href)="([^"]+)"',html)
        for url in urls:
            if url.startswith(('http:','https:','data:','#')): continue
            path=ROOT/'web'/url.split('?')[0].lstrip('/')
            self.assertTrue(path.is_file(),url)
        self.assertNotIn('cdn.jsdelivr.net',html)

    def test_runtime_has_no_hosted_service_dependencies(self):
        source='\n'.join((ROOT/path).read_text(encoding='utf-8') for path in ['server/src/main.rs','server/src/mcp.rs','server/src/ai_gateway.rs'])
        for marker in ['UNICELL_PUBLIC_ORIGIN','production_state::','ai_quota::','omnidoc_connect::','shared_workbooks::','cloud_storage::']:
            self.assertNotIn(marker,source)
        config=(ROOT/'server/src/ai_gateway.rs').read_text(encoding='utf-8')
        for marker in ['UNIPPT_AI_KEY','UNIMAIL_AI_KEY','UNIDOC_AI_KEY']:
            self.assertNotIn(marker,config)

if __name__ == '__main__': unittest.main()
