// Offline helper; npm install --prefix .local/prop-tools meshoptimizer@1.0.1
import fs from 'node:fs';
import { MeshoptSimplifier, MeshoptEncoder } from '../.local/prop-tools/node_modules/meshoptimizer/index.js';
await Promise.all([MeshoptSimplifier.ready, MeshoptEncoder.ready]);
const input = JSON.parse(fs.readFileSync(0, 'utf8'));
const positions = new Float32Array(input.vertices.flatMap(v => v.slice(0, 3)));
const attributes = new Float32Array(input.vertices.flatMap(v => v.slice(3, 8)));
const [indices, error] = MeshoptSimplifier.simplifyWithAttributes(
  new Uint32Array(input.indices), positions, 3, attributes, 5,
  [0.15, 0.15, 0.15, 0.3, 0.3], null, input.target * 3, input.error, ['Prune']);
const [remap, count] = MeshoptEncoder.reorderMesh(indices, true, false);
const vertices = new Array(count);
for (let i = 0; i < remap.length; ++i)
  if (remap[i] !== 0xffffffff) vertices[remap[i]] = input.vertices[i];
process.stdout.write(JSON.stringify({vertices, indices: Array.from(indices), error}));
