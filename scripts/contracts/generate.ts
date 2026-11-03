/** Regenerate synthetic golden files using the pinned upstream oracle. No credentials. */
import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
const revision = "0408add4c47a8549c9ad76c680d9a5779a58a050";
const root = resolve(import.meta.dir, "../..");
const output = `${root}/crates/civpulse-results-contract/tests/fixtures`;
const temp = await mkdtemp(`${tmpdir()}/civpulse-oracle-`);
try {
  const files = ["apps/results-collector/src/feed.ts", "apps/results-collector/src/collector.ts", "apps/results-collector/src/storage.ts", "scripts/sos-results.ts"];
  for (const file of files) {
    const response = await fetch(`https://raw.githubusercontent.com/kerryhatcher/www.civpulse.org/${revision}/${file}`);
    if (!response.ok) throw Error(`Oracle download failed: ${file}`);
    await mkdir(`${temp}/${file.slice(0, file.lastIndexOf("/"))}`, { recursive: true });
    await Bun.write(`${temp}/${file}`, await response.text());
  }
  const { prepare, canonical } = await import(`${temp}/apps/results-collector/src/feed.ts`);
  const { writeAssets } = await import(`${temp}/apps/results-collector/src/collector.ts`);
  const option = (id: string, voteCount: number | null) => ({ id, name: `Choice ${id}`, voteCount, groupResults: [{groupName:"Election Day", voteCount}], precinctResults:[{id:"p1", voteCount}] });
  const scope = (id: string) => ({id, name: `Scope ${id}`, reportingStatuses:null, ballotItems:[{id:"race-1", name:"Example race", voteFor:1, rankedChoiceResults:[], ballotOptions:[option("A",12), option("b",0), option("c",null), {id:"d",name:"Unknown count"}]}]});
  const base = { electionDate:"2026-11-03", electionName:"Synthetic contract election", createdAt:"2026-11-03T10:00:00Z", results:scope("GA"), localResults:[scope("001")] };
  const variants: Record<string, any> = {base};
  variants.reordered = structuredClone(base);
  variants.reordered.createdAt = "2026-11-03T10:01:00Z";
  variants.reordered.results.ballotItems[0].ballotOptions.reverse();
  variants.correction = structuredClone(base);
  variants.correction.results.ballotItems[0].ballotOptions[0].voteCount = 11;
  variants.label = structuredClone(base);
  variants.label.results.ballotItems[0].ballotOptions[0].name = "Corrected label";
  variants.status = structuredClone(base);
  variants.status.results.reportingStatuses = [];
  const expected: Record<string, any> = {reference_commit:revision, cases:{}};
  for (const [name, source] of Object.entries(variants)) {
    const raw = name === "reordered" ? "\uFEFF  " + JSON.stringify(source,null,2) + "\n" : JSON.stringify(source);
    await Bun.write(`${output}/${name}.json`, raw);
    const prepared = await prepare(`${output}/${name}.json`, `${temp}/${name}`, "2026-11-03");
    expected.cases[name] = {fingerprint:prepared.fingerprint, raw_sha256:new Bun.CryptoHasher("sha256").update(raw).digest("hex"), item_count:prepared.itemCount, jurisdictions:prepared.jurisdictions};
    if (name !== "base") continue;
    const election = "00000000-0000-4000-8000-000000000001", snapshot = "00000000-0000-4000-8000-000000000002";
    const objects = new Map<string,string>();
    const store = {url:(key:string)=>`https://files.civpulse.org/${key}`, put:async(key:string,body:string)=>{ objects.set(key,body); return new Response(null,{status:200}); }, get:async(key:string)=>new Response(objects.get(key))};
    const receipt = {source_url:"https://results.sos.ga.gov/cdn/results/Georgia/export-11032026GeneralAndSpecialElection.json", sha256:expected.cases.base.raw_sha256, bytes:new TextEncoder().encode(raw).length, raw_url:store.url(`elections/${election}/raw/${expected.cases.base.raw_sha256}.json`), retrieved_at:"2026-11-03T10:00:10Z",etag:null,last_modified:null};
    const {manifest} = await writeAssets(store,election,snapshot,prepared,receipt);
    expected.assets = [];
    for (const [key,body] of objects) {
      const file = "assets/" + key.split(`/revisions/${snapshot}/`)[1];
      await mkdir(`${output}/${file.slice(0,file.lastIndexOf("/"))}`,{recursive:true});
      await Bun.write(`${output}/${file}`,body);
      expected.assets.push({key,file,sha256:new Bun.CryptoHasher("sha256").update(body).digest("hex")});
    }
    await Bun.write(`${output}/latest.json`,JSON.stringify({...manifest,schema_version:1,revision:"9007199254740993",published_at:"2026-11-03T10:00:20Z"}));
    const canonicalScope = canonical(source.results);
    await Bun.write(`${output}/canonical-scope.json`,canonicalScope);
    expected.canonical_scope_sha256 = new Bun.CryptoHasher("sha256").update(canonicalScope).digest("hex");
  }
  await Bun.write(`${output}/expected.json`,JSON.stringify(expected,null,2)+"\n");
} finally { await rm(temp,{recursive:true,force:true}); }
