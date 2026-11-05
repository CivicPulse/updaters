#!/usr/bin/env python3
"""Authenticated catalog/read parity; only optional mutation is the test user's own profile."""
import argparse
import json
import os
import urllib.error
import urllib.request
from pathlib import Path
from urllib.parse import urlencode

parser = argparse.ArgumentParser()
parser.add_argument('--base', default='https://webapi.civpulse.org')
parser.add_argument('--mcp-base')
parser.add_argument('--test-own-profile', action='store_true')
args = parser.parse_args()
token = os.environ['MCP_SMOKE_TOKEN']
root = Path(__file__).resolve().parents[3]
reference = root / 'crates/civpulse-civic/reference'
expected = json.loads((reference / 'tools.json').read_text()) + json.loads((reference / 'extensions.json').read_text())
manifest = json.loads((reference / 'resources.json').read_text())


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *_args, **_kwargs):
        return None

opener = urllib.request.build_opener(NoRedirect)

def request(path, body=None, method=None, auth=True):
    headers = {'User-Agent': 'curl/8.18.0', 'Content-Type': 'application/json',
               'Accept': 'application/json, text/event-stream'}
    if auth:
        headers['Authorization'] = 'Bearer ' + token
    base = (args.mcp_base or args.base) if path.startswith('/mcp/') else args.base
    req = urllib.request.Request(base.rstrip('/') + path,
        data=None if body is None else json.dumps(body).encode(), headers=headers, method=method)
    try:
        response = opener.open(req, timeout=90)
    except urllib.error.HTTPError as error:
        response = error
    assert response.headers.get('cache-control') == 'no-store'
    data = response.read()
    return response.status, json.loads(data) if data else None


def rpc(method, params):
    status, body = request('/mcp/v2', {'jsonrpc': '2.0', 'id': 1, 'method': method, 'params': params})
    assert status == 200, (method, status)
    assert 'error' not in body, body
    return body['result']


def tool(name, arguments):
    result = rpc('tools/call', {'name': name, 'arguments': arguments})
    if result.get('isError'):
        return False, None
    return True, json.loads(result['content'][0]['text'])


for path, body in [('/api/v1/operations', None), ('/api/v1/civic-resources', None),
                   ('/api/v1/data/people', None),
                   ('/mcp/v2', {'jsonrpc': '2.0', 'id': 1, 'method': 'tools/list', 'params': {}})]:
    assert request(path, body, auth=False)[0] == 401
assert request('/api/v1/operations')[1]['operations'] == expected
assert rpc('tools/list', {})['tools'] == expected
assert request('/api/v1/civic-resources')[1] == manifest
assert tool('list_civic_resources', {}) == (True, manifest)

checked = 0
workflow_only = 0
for resource in manifest['resources']:
    name = resource['name']
    parameters = {'resource': name, 'limit': 1}
    status, api = request('/api/v1/operations/search_civic_data', parameters)
    ok, mcp = tool('search_civic_data', parameters)
    if resource.get('record_type') or (resource['table_operations']['read'] and not resource.get('read_requires_admin')):
        assert status == 200 and ok, (name, status)
        assert api == mcp, 'Read parity mismatch: ' + name
        checked += 1
    else:
        assert status == 403 and not ok, (name, status)
        workflow_only += 1
    print(json.dumps({'resource': name, 'parity': True, 'direct_read': status == 200}), flush=True)

for name, parameters in [
    ('search_records_requests', {'query': '', 'managed': True}),
    ('get_sos_results', {'election_id': '295d864d-80c8-4a87-ab16-3f4f3fa5ddb7'}),
    ('get_sos_results_mappings', {'election_id': '295d864d-80c8-4a87-ab16-3f4f3fa5ddb7'}),
    ('search_civic_data', {'resource': 'offices', 'query': 'No match, \"quoted\" (literal)', 'limit': 1}),
    ('get_object_history', {'limit': 1}),
    ('list_public_documents', {'limit': 1}),
    ('school_finances', {'operation': {'action': 'browse', 'limit': 1}}),
]:
    status, api = request('/api/v1/operations/' + name, parameters)
    ok, mcp = tool(name, parameters)
    assert status == 200 and ok and api == mcp, (name, status)

status, identity = request('/api/v1/operations/whoami', {})
assert status == 200 and identity['is_admin'] is False
assert request('/api/v1/operations/get_voter_roll_history', {'registration_number': '123'})[0] == 403
assert tool('get_voter_roll_history', {'registration_number': '123'})[0] is False
if args.test_own_profile:
    parameters = {'resource': 'profiles', 'keys': {'id': identity['user_id']},
                  'data': {'display_name': 'Civic parity smoke'}}
    assert request('/api/v1/operations/update_civic_data', parameters)[0] == 200
    assert tool('update_civic_data', parameters)[0] is True
print(json.dumps({'passed': True, 'catalog_tools': len(expected),
    'resources': len(manifest['resources']), 'direct_reads_checked': checked,
    'workflow_only_denials_checked': workflow_only,
    'own_profile_tested': args.test_own_profile}), flush=True)
