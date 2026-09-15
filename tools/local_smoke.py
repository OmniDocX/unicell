"""Exercise a running local edition using fresh, isolated workbook sessions."""
import argparse
import http.cookiejar
import json
import urllib.error
import urllib.parse
import urllib.request

class Client:
    def __init__(self, base):
        self.base = base.rstrip('/')
        self.cookies = http.cookiejar.CookieJar()
        self.opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(self.cookies))

    def request(self, path, body=None, method=None, headers=None):
        request_headers = dict(headers or {})
        if isinstance(body, (dict, list)):
            body = json.dumps(body).encode()
            request_headers['Content-Type'] = 'application/json'
        request = urllib.request.Request(self.base + path, data=body, method=method, headers=request_headers)
        try:
            response = self.opener.open(request, timeout=90)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            return response.status, dict(response.headers), response.read()

    def json(self, path, body=None, method=None):
        status, _, data = self.request(path, body, method)
        assert status == 200, (path,status,data[:200])
        result = json.loads(data)
        assert result.get('ok') is not False, result
        return result

def run(base):
    parsed = urllib.parse.urlparse(base)
    assert parsed.scheme == 'http' and parsed.hostname in ('127.0.0.1','localhost','::1'), 'Use a local server'
    a, b = Client(base), Client(base)
    report = []
    a.json('/api/session'); b.json('/api/session')
    a.json('/api/new', {}, 'POST'); b.json('/api/new', {}, 'POST')
    a.json('/api/import-csv', '名称,数量,计算\r\n测试,7,=3*4\r\n'.encode())
    cell = '/api/cell?sheet=0&row=2&col=3'
    assert a.json(cell)['formatted'] == '12'
    assert b.json(cell)['content'] == ''
    report.append('CSV formula computation and session isolation')
    a.json('/api/input', {'sheet':0,'row':2,'col':2,'value':'19'})
    assert a.json('/api/cell?sheet=0&row=2&col=2')['value'] == 19
    a.json('/api/undo',{},'POST')
    assert a.json('/api/cell?sheet=0&row=2&col=2')['value'] == 7
    a.json('/api/redo',{},'POST')
    report.append('Cell edits, undo and redo')
    for export_path, import_path, signature in [
        ('/api/export','/api/import',b'PK'),
        ('/api/export-udoc','/api/import-udoc',None),
        ('/api/export-html','/api/import-html',None),
    ]:
        status, _, data = a.request(export_path)
        assert status == 200 and data, (export_path,status)
        if signature: assert data.startswith(signature)
        b.json(import_path,data)
        assert b.json(cell)['content'] == '=3*4', import_path
        assert b.json(cell)['formatted'] == '12', import_path
        report.append(export_path + ' round trip')
    status, _, csv = a.request('/api/export-csv?sheet=0')
    assert status == 200 and '测试,19,12' in csv.decode('utf-8-sig')
    report.append('CSV display-value export')
    def rpc(method, params=None):
        return a.json('/mcp', {'jsonrpc':'2.0','id':1,'method':method,'params':params or {}})
    assert rpc('initialize',{'protocolVersion':'2025-06-18','capabilities':{},'clientInfo':{'name':'smoke','version':'1'}})['result']['protocolVersion'] == '2025-06-18'
    assert a.request('/mcp',{'jsonrpc':'2.0','method':'notifications/initialized'})[0] == 202
    names = {x['name'] for x in rpc('tools/list')['result']['tools']}
    assert names == {'workbook_info','read_cell','read_range','write_cell','format_range'}
    result = rpc('tools/call',{'name':'write_cell','arguments':{'sheet':0,'cell':'D2','value':'=B2*2'}})
    assert result['result']['structuredContent']['ok'] is True
    assert a.json('/api/cell?sheet=0&row=2&col=4')['value'] == 38
    a.json('/api/undo',{},'POST')
    assert a.json('/api/cell?sheet=0&row=2&col=4')['content'] == ''
    report.append('MCP handshake, discovery, write and undo')
    for path in ['/api/auth/status','/api/collaboration','/api/workbook-share','/api/ai/limits','/cloud/list','/.well-known/oauth-authorization-server']:
        assert a.request(path)[0] == 404, path
    assert a.request('/api/info',headers={'Origin':'https://outside.example'})[0] == 403
    assert a.request('/api/info',headers={'Host':'outside.example'})[0] == 403
    assert a.request('/../server/Cargo.toml')[0] in (400,404)
    report.append('Removed hosted routes, cross-origin and path boundaries')
    status, headers, html = a.request('/')
    assert status == 200 and 'no-store' in headers['Cache-Control']
    assert all(x not in html for x in [b'btn-account',b'btn-cloud',b'btn-share-workbook',b'collaboration-editor'])
    assert a.request('/vendor/katex/katex.min.js')[0] == 200
    assert a.json('/fonts/manifest.json')['schemaVersion'] == 1
    report.append('Local frontend and offline assets')
    return {'ok':True,'checks':report}

if __name__ == '__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--url',default='http://127.0.0.1:8143')
    print(json.dumps(run(parser.parse_args().url),ensure_ascii=False,indent=2))
