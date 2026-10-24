// Offline helper; npm install --prefix .local/prop-tools meshoptimizer@1.0.1
import fs from 'node:fs';
import {
  MeshoptSimplifier,
  MeshoptEncoder,
} from '../.local/prop-tools/node_modules/meshoptimizer/index.js';

await Promise.all([MeshoptSimplifier.ready, MeshoptEncoder.ready]);

// Vertices contain position XYZ, normal XYZ, and UV. Weight UV preservation
// more heavily than normals to keep texture seams stable during simplification.
const input = JSON.parse(fs.readFileSync(0, 'utf8'));
const positions = new Float32Array(input.vertices.flatMap(vertex => vertex.slice(0, 3)));
const attributes = new Float32Array(input.vertices.flatMap(vertex => vertex.slice(3, 8)));
const attributeWeights = [0.15, 0.15, 0.15, 0.3, 0.3];
const [indices, error] = MeshoptSimplifier.simplifyWithAttributes(
  new Uint32Array(input.indices),
  positions,
  3,
  attributes,
  5,
  attributeWeights,
  null,
  input.target * 3,
  input.error,
  ['Prune'],
);

// reorderMesh updates indices in place and maps used source vertices to a
// compact output buffer. Unused vertices have the sentinel index 0xffffffff.
const [remap, count] = MeshoptEncoder.reorderMesh(indices, true, false);
const vertices = new Array(count);
for (let index = 0; index < remap.length; ++index) {
  if (remap[index] !== 0xffffffff) {
    vertices[remap[index]] = input.vertices[index];
  }
}
process.stdout.write(JSON.stringify({ vertices, indices: Array.from(indices), error }));
