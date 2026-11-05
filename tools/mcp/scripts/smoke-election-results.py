#!/usr/bin/env python3
"""Read-only parity check. Supply an ordinary user's MCP_SMOKE_TOKEN in the environment."""
import argparse
import json
import os
import urllib.request
from urllib.parse import urlencode

parser = argparse.ArgumentParser()
parser.add_argument('--base', default='https://webapi.civpulse.org')
parser.add_argument('--election-id', default='295d864d-80c8-4a87-ab16-3f4f3fa5ddb7')
parser.add_argument('--query', action='append')
args = parser.parse_args()
token = os.environ['MCP_SMOKE_TOKEN']


def read(path, body=None):
    request = urllib.request.Request(args.base.rstrip('/') + path,
        data=None if body is None else json.dumps(body).encode(),
        headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json',
                 'Accept': 'application/json, text/event-stream', 'User-Agent': 'curl/8.18.0'})
    with urllib.request.urlopen(request, timeout=90) as response:
        assert response.status == 200
        assert response.headers.get('cache-control') == 'no-store'
        return json.load(response)


for query in args.query or ['DeFranco', 'McLaurin']:
    api = read('/api/v1/elections/' + args.election_id + '/results?' + urlencode({'query': query}))
    rpc = read('/mcp/v2', {'jsonrpc': '2.0', 'id': 1, 'method': 'tools/call',
        'params': {'name': 'read_official_election_results',
                   'arguments': {'election_id': args.election_id, 'query': query}}})
    result = rpc['result']
    assert not result.get('isError'), result
    mcp = json.loads(result['content'][0]['text'])
    assert api['source_sha256'] == mcp['source_sha256'], 'Source changed between reads; rerun against one snapshot'
    assert api['contests'] == mcp['contests']
    assert api['contests'], 'No matching contest'
    assert api['certification_status'] == mcp['certification_status'] == 'not_established'
    print(json.dumps({'query': query, 'source_updated_at': api['source_updated_at'],
        'source_sha256': api['source_sha256'], 'contests': api['contests']}))

for path in ['/api/v1/elections?' + urlencode({'query': 'November'}),
             '/api/v1/people?' + urlencode({'query': 'DeFranco'}),
             '/api/v1/people?' + urlencode({'query': 'McLaurin'})]:
    assert read(path), 'Discovery search returned no records'
print('MCP/API result parity and discovery checks passed')
