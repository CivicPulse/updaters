// Run with Bun. Uses only in-memory MCP and mocked HTTP; never touches a database.
// Usage: bun tools/mcp/scripts/export-reference.ts /path/to/www.civpulse.org
import { resolve } from 'node:path';
const repo=resolve(Bun.argv[2]??'');
const {buildServer}=await import(`${repo}/apps/mcp/src/tools.ts`);
const {Client}=await import(`${repo}/apps/mcp/node_modules/@modelcontextprotocol/sdk/dist/esm/client/index.js`);
const {InMemoryTransport}=await import(`${repo}/apps/mcp/node_modules/@modelcontextprotocol/sdk/dist/esm/inMemory.js`);
const {createClient}=await import(`${repo}/apps/mcp/node_modules/@supabase/supabase-js/dist/index.mjs`);
const {legislativeResources}=await import(`${repo}/apps/web/src/lib/legislation.ts`);
const {structuredTurnout}=await import(`${repo}/apps/mcp/src/structured-turnout.ts`);
const {z}=await import(`${repo}/apps/mcp/node_modules/zod/index.js`);
const id='00000000-0000-4000-8000-000000000001';
const user={id,email:'test@example.org'};
const root=resolve(import.meta.dir,'..');
async function connection(api:any){const server=buildServer(api,user,{},undefined,'compact');const client=new Client({name:'rust-port-reference',version:'1'});const [a,b]=InMemoryTransport.createLinkedPair();await server.connect(a);await client.connect(b);return {client,close:async()=>{await client.close();await server.close();}};}
const {McpServer}=await import(`${repo}/apps/mcp/node_modules/@modelcontextprotocol/server/dist/index.mjs`);
const originalRegister=McpServer.prototype.registerTool;
const normalization:Record<string,string[][]>={};
function trimPaths(schema:any,path:string[]=[],out:string[][]=[]):string[][]{
 const d=schema?._zod?.def;if(!d)return out;
 if(d.type==='string'&&d.checks?.some((c:any)=>c._zod.def.check==='overwrite'&&c._zod.def.tx(' x ')==='x'))out.push(path);
 if(d.type==='object')for(const [k,s]of Object.entries(d.shape))trimPaths(s,[...path,k],out);
 else if(d.type==='array')trimPaths(d.element,[...path,'*'],out);
 else if(d.options)for(const s of d.options)trimPaths(s,path,out);
 else if(d.innerType)trimPaths(d.innerType,path,out);
 return out;
}
McpServer.prototype.registerTool=function(name:any,config:any,...args:any[]){normalization[name]=trimPaths(config.inputSchema);return originalRegister.call(this,name,config,...args);};
const c=await connection({});const tools=(await c.client.listTools()).tools;await c.close();
await Bun.write(`${root}/reference/normalization.json`,JSON.stringify(normalization,null,2)+'\n');
await Bun.write(`${root}/reference/tools.json`,JSON.stringify(tools,null,2)+'\n');
await Bun.write(`${root}/reference/legislation.json`,JSON.stringify(legislativeResources,null,2)+'\n');
await Bun.write(`${root}/reference/turnout.json`,JSON.stringify(z.toJSONSchema(structuredTurnout),null,2)+'\n');
function sample(s:any,key=''):any{
 if(Array.isArray(s.type))return sample({...s,type:s.type.find((t:string)=>t!=='null')},key);
 if(s.pattern==='^[1-9][0-9]*$')return '1';
 if(key==='retrieved_on')return '2026-01-01';
 if(key==='url')return 'https://example.org/source';
 if('default'in s)return s.default;if('const'in s)return s.const;if(s.enum)return s.enum[0];if(s.anyOf)return sample(s.anyOf.find((v:any)=>v.type!=='null'),key);if(s.oneOf)return sample(s.oneOf[0],key);
 if(s.type==='object')return Object.fromEntries((s.required??[]).map((k:string)=>[k,sample(s.properties[k],k)]));
 if(s.type==='array')return Array.from({length:s.minItems??0},()=>sample(s.items));if(s.type==='boolean')return true;
 if(s.type==='number'||s.type==='integer')return s.minimum??(s.exclusiveMinimum!==undefined?s.exclusiveMinimum+1:1);
 if(s.format==='uuid'||key.endsWith('_id')||key==='id')return id;
 if(s.format==='date')return '2026-01-01';if(s.format==='date-time')return '2026-01-01T00:00:00Z';if(s.format==='uri'||key.endsWith('_url'))return 'https://example.org/source';
 if(key==='county_fips')return '13021';if(key.includes('sha256'))return 'a'.repeat(64);if(s.pattern==='^[1-9][0-9]*$')return '1';
 return 'sample'.padEnd(s.minLength??1,'x');
}
const cases:any[]=[];
function add(name:string,args:any){cases.push({name,args});}
const skip=new Set(['import_meeting_source','begin_meeting_attachment_upload','finalize_meeting_attachment_upload','manage_meeting_attachment_upload','upload_meeting_attachment','get_meeting_attachment_url','get_georgia_legislative_source','search_document_contents','publish_meeting_document','withdraw_public_document','manage_election_results','upload_image']);
for(const t of tools){if(skip.has(t.name))continue;
 if(t.name.startsWith('save_')&&t.inputSchema.properties.request){
  const request=t.inputSchema.properties.request;
  if(t.name==='save_relationship'){for(const r of request.oneOf??request.anyOf){for(const change of r.properties.change.oneOf??r.properties.change.anyOf){const x=sample(r);x.change=sample(change);if(x.change.operation==='update'){const key=Object.keys(change.properties).find(k=>!['id','operation','map_id','layer_id','contest_id','person_id','organization_id','article_id'].includes(k));if(key)x.change[key]=sample(change.properties[key],key);}add(t.name,{request:x});}}}
  else {for(const r of request.oneOf??request.anyOf){const x=sample(r);if(x.operation==='update'){const key=Object.keys(r.properties).find(k=>!['id','operation','layer_id'].includes(k));if(key)x[key]=sample(r.properties[key],key);}add(t.name,{request:x});}}
 }else if(['delete_relationship','read_related_records'].includes(t.name)){for(const r of t.inputSchema.properties.request.oneOf??t.inputSchema.properties.request.anyOf){const x=sample(r);if(x.relation==='office_terms')x.person_id=id;add(t.name,{request:x});}}
 else{let a=sample(t.inputSchema);if(t.name==='get_civic_record')a.type='person';if(t.name==='save_postgis_layer')a.name='Example';if(t.name==='save_legislative_record')a.data=sampleLegislation(a.table);add(t.name,a);}
}
function sampleLegislation(table:string){return Object.fromEntries(legislativeResources[table].fields.filter((f:any)=>f.required&&!f.default).map((f:any)=>[f.name,f.reference?id:f.choices?.[0]??(f.type==='integer'?1:'sample')]));}
for(const type of ['organization','person','map','election','event'])add('search_site',{mode:'directory',type,query:'sample'});
for(const type of ['organization','map','event','election','article'])add('get_civic_record',{type,id});
add('search_site',{type:'county_statistic'});add('search_site',{type:'county_statistic_catalog'});
add('school_finances',{operation:{action:'begin',payload:{test:true}}});
add('import_legislation',{records:Array.from({length:201},()=>({table:'bills',data:{}})),mode:'batched'});
add('list_public_documents',{query:'sample',include_unpublished:true,meeting_id:id,limit:5,offset:2});
for(const action of ['read','read_turnout'])add('manage_election_results',{operation:{action,election_id:id}});
add('get_layer_feature',{layer_id:id,id:'1'});
add('save_county_statistic',{county_fips:'13021',indicator_code:'sample',expected_updated_at:null,value:42,status:'available',period:'2026',source_id:'sample',source_variables:'sample'});
add('search_users',{query:'  sample  '});
add('save_person',{request:{operation:'update',id,full_name:' Name '}});
add('save_article',{request:{operation:'create',title:'  Title  ',url:'https://example.org/article'}});
add('import_meeting_source',{meeting_id:id,provider:'test',provenance:{source_url:'https://example.org/source',document_sha256:'a'.repeat(64)},attendance:[{source_key:'a',locator:'page 1',person_id:id,status:'observed',evidence:'Named ballot'}],motions:[{source_key:'vote',locator:'page 2',subject:'Motion',ballots:[{person_id:id,choice:'yes',text:'Sample: Yes'}]}]});
add('import_meeting_source',{meeting_id:id,provider:'test',provenance:{source_url:'https://example.org/source',document_sha256:'a'.repeat(64)},reconcile_only:true});
const {parseSimbliMinutes}=await import(`${repo}/apps/mcp/src/simbli-minutes.ts`);
const sourceURL='https://example.org/source';
const doc=(items:any[])=>JSON.stringify([{document:{role:'Minutes',source_url:sourceURL},public_content:{LstItemMinutes:items}}]);
const sourceDocument=doc([{EncrId:'item1',Title:'Budget',MeetingOnlineVotings:[{Minutes:'Approve budget',VotingHTML:['<p>Yes: Alice</p><p>Vote Not Recorded: Bob</p><p>Absent: Carol</p>']}]}]);
const documentHash=new Bun.CryptoHasher('sha256').update(sourceDocument).digest('hex');
const people=[{name:'Alice',person_id:id},{name:'Bob',person_id:'00000000-0000-4000-8000-000000000002'},{name:'Carol',person_id:'00000000-0000-4000-8000-000000000003'}];
for(const variant of ['import','missing','inventory','unbound'])cases.push({name:'import_meeting_source',document:sourceDocument,meetingSources:variant==='unbound'?[{kind:'vote',provider:'test',source_key:'legacy',vote_id:id,data:{subject:'Budget',description:'Approve budget'},provenance:{document_sha256:documentHash}}]:[],args:{meeting_id:id,provider:'test',provenance:{source_url:sourceURL,document_sha256:documentHash},attachment:{id,adapter:'simbli-minutes-v1',people:variant==='missing'?people.slice(0,1):people},reconcile_only:variant==='inventory'}});
const parserInputs=[sourceDocument,doc([{EncrId:'two',Title:'Items',MeetingOnlineVotings:[{Minutes:'Motion one\nVoting:\nAlice: Yes\nBob: No\nMotion two\nVoting:\nAlice: Abstained'}]}]),doc([{Title:'Conflict',MeetingOnlineVotings:[{Minutes:'Approve\nVoting:\nAlice: Yes',VotingHTML:['<p>No: Alice</p>']}]}]),doc([{Visibility:1,Title:'Private',MeetingOnlineVotings:[{Minutes:'Voting: Alice: Yes'}]},{Title:'Parent',ChildMinutesLst:[{Title:'Child',MeetingOnlineVotings:[{Minutes:'Motion without named votes'}]}]}]),doc([{Title:'Deleted',MeetingOnlineVotings:[{IsDeleted:true}]}]),doc([{Title:'Unknown',MeetingOnlineVotings:[{VotingHTML:['<p>Yes: Alice</p><p>Mystery: Bob</p><p>A motion followed</p>']}]}]),doc([{Title:'Duplicate',MeetingOnlineVotings:[{VotingHTML:['<p>Yes: Alice</p><p>No: Alice</p>']}]}])];
await Bun.write(`${root}/tests/simbli.json`,JSON.stringify(parserInputs.map(input=>{try{return {input,result:parseSimbliMinutes(input)}}catch(e){return {input,error:true}}}),null,2)+'\n');
const fixtures:any[]=[];
for(const test of cases){
 const requests:any[]=[];
 const api=createClient('http://localhost:54321','test-public-key',{auth:{persistSession:false,autoRefreshToken:false},global:{headers:{Authorization:'Bearer caller-token'},fetch:async(input:any,init:any)=>{
  const request=new Request(input,init);const url=new URL(request.url);let body:any=null;try{body=await request.json()}catch{}
  let response:any;
  if(url.pathname.endsWith('/is_admin'))response=true;
  else if(url.pathname.endsWith('/get_person'))response={status:'ok',person:{id},organizations:[{id}]};
  else if(url.pathname.endsWith('/get_election'))response={status:'ok',election:{id},contests:[{id}],ballot_questions:[{id}]};
  else if(url.pathname.endsWith('/get_meeting'))response={status:'ok',sources:test.meetingSources??[],attachments:test.document?[{id,meeting_id:id,mime_type:'application/json',size_bytes:Buffer.byteLength(test.document),storage_path:`${id}/source.json`,sha256:documentHash}]:[]};
  else if(url.pathname.endsWith('/import_meeting_records_compact'))response={status:'complete',errors:[]};
  else if(url.pathname.endsWith('/get_map'))response={status:'ok',map:{id}};
  else if(url.pathname.endsWith('/mcp_get_record'))response={status:'ok',contest:{candidates:[{person:{id}}]}};
  else if(url.pathname.endsWith('/mcp_layer_features'))response=[{id:'1'}];
  else if(url.pathname.endsWith('/mcp_save_article')||url.pathname.endsWith('/mcp_save_postgis_layer')||url.pathname.endsWith('/mcp_save_layer_feature'))response=id;
  else if(url.pathname.includes('/rpc/'))response=[{id}];
  else if(url.pathname.includes('/storage/v1/'))response=[];
  else response=request.headers.get('accept')?.includes('vnd.pgrst.object')?{id}:[{id}];
  const query=Object.fromEntries(url.searchParams);if(request.headers.has('range')){const [lo,hi]=request.headers.get('range')!.split('-').map(Number);query.offset=String(lo);query.limit=String(hi-lo+1);}
  if(test.document&&url.pathname.includes('/object/')){requests.push({method:request.method,path:url.pathname,query,body,response_base64:Buffer.from(test.document).toString('base64')});return new Response(test.document);}
  requests.push({method:request.method,path:url.pathname,query,body,response});return Response.json(response);
 }}});
 const c=await connection(api);let result:any;
 try{result=await c.client.callTool({name:test.name,arguments:test.args});}finally{await c.close();}
 if(result.isError){console.error(test.name,JSON.stringify(test.args),result.content);continue;}
 fixtures.push({...test,requests,result:JSON.parse(result.content[0].text)});
}
await Bun.write(`${root}/tests/oracle.json`,JSON.stringify(fixtures,null,2)+'\n');console.log(`Exported ${tools.length} tools and ${fixtures.length} successful compatibility cases`);
