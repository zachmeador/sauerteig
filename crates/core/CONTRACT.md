# Shared first-playable contract

All public types below are re-exported from sauerteig_core, derive Clone, Debug,
Serialize and Deserialize (as applicable), and use glam 0.29 with serde. Public
protocol enums use #[serde(tag = "type", rename_all = "snake_case")]. Vec3 uses
glam serde's [x,y,z] representation. The implementation in `src/` is authoritative; keep this overview aligned
when changing shared types.

## World

Constants CHUNK_SIZE: i32 = 8, WORLD_MIN: i32 = -64, WORLD_MAX: i32 = 64,
WORLD_HEIGHT: i32 = 48, PLAYER_RADIUS: f32 = 0.3, PLAYER_HEIGHT: f32 = 1.75,
EYE_HEIGHT: f32 = 1.55, FIXED_DT: f32 = 1.0 / 60.0.

type CellPos = [i32; 3]; type ChunkKey = [i32; 3].
Cell { pub material: u8, pub heights: [u8;4], pub diagonal:bool,
       pub shape:Option<Shape> } is Copy+PartialEq+Eq.
Heights 0..8 give top height /8 within the unit cell. Corner order is
(x0,z0), (x1,z0), (x1,z1), (x0,z1). diagonal=false joins corners0->2;
true joins1->3. Old saves omit diagonal and shape and decode unchanged.
Shape { vertices:[[u8;3];8], diagonals:u8 } replaces heights when present.
Vertices are local eighths: four bottom corners in the same X/Z order, then
four corresponding top corners. SHAPE_FACES defines outward quad winding for
-X,+X,-Y,+Y,-Z,+Z. Each face's diagonal bit chooses a-c (0) or b-d (1).
General shapes must have a valid oriented closed boundary with positive volume;
folds and non-manifold pinches are rejected. Old height-field pinches retain
their original representation and behavior.
Cell::solid(material:u8)->Cell. MATERIAL_COLORS: [[f32;3];8] in this order:
concrete, gunmetal, rusted steel, worn concrete, hazard paint, amber light (material5), rock, earth. MATERIAL_NAMES mirrors these stable slots.
CellEdit { pub pos:CellPos, pub cell:Option<Cell> }.
World holds sparse 8-cube chunks; empty cells omitted from serialized snapshots.
World::empty()->Self; World::arena()->Self;
World::from_cells(cells:&[CellEdit])->Self;
World::get(pos:CellPos)->Option<Cell>;
World::set(pos:CellPos,cell:Option<Cell>);
World::cells()->Vec<CellEdit>;
World::chunk_keys()->Vec<ChunkKey>;
World::apply_edits(edits:&[CellEdit])->Vec<ChunkKey> returns changed chunks AND
their face neighbors for renderer invalidation, deduplicated.
World::is_valid_pos(pos:CellPos)->bool is associated static function.
World::cell_chunk(pos:CellPos)->ChunkKey is associated static function.
World::mesh_chunk(key:ChunkKey)->Mesh;
World::raycast(origin:Vec3,direction:Vec3,max_distance:f32)->Option<RayHit>;
RayHit { pub pos:CellPos, pub bounds:FineBox, pub face:u8, pub normal:Vec3,
         pub point:Vec3, pub distance:f32 }. face is the logical face even when its normal slopes.
Mesh { pub vertices:Vec<Vertex>, pub indices:Vec<u32> };
Vertex { pub position:[f32;3], pub normal:[f32;3], pub color:[f32;3], pub material:u8 }.
Empty/internal full-solid faces are culled. General shapes retain some partially
occluded triangles; opaque depth testing hides their covered portions. Rays
reject internal union boundaries. Rendering, rays, and collision use the same
explicit triangles, including concave shapes. Geometric normals and material
colors are per face. Height-field cells retain their fast collision path;
general cells use triangle/box SAT and containment plus clipped floor/ceiling
triangles.

Fine geometry retains one-metre roots and allocates sparse fragments only when
needed. Coordinates in FineBox and CellFragment origin/size are integer eighths.
FineBox { min:[i32;3], max:[i32;3] } is half-open.
CellFragment { bounds:FineBox, origin:[i32;3], size:u32, cell:Cell } clips an authored
source cell (power-of-two size1..65536 eighths) to bounds. The 8192-metre
source limit supports imported octree leaves; editor creation grids remain unchanged.
Checked source bounds and world-space collision tolerances preserve small clips. Source geometry remains unchanged,
including triangle diagonals and old height-field pinches. Clip caps are
triangulated exactly; cached boundaries serve meshes, rays, and support queries.
RootEdit { pos:CellPos, fragments:Vec<CellFragment> } atomically replaces one root.
Fragment bounds in a root must be disjoint, valid, and contained in that root;
at most512 fragments are allowed per root.
World::from_parts(cells:&[CellEdit],roots:&[RootEdit])->Result<World,String>;
World::root_fragments(pos:CellPos)->Vec<CellFragment>;
World::detailed_roots()->Vec<RootEdit>;
World::roots_in(bounds:FineBox)->Vec<RootEdit> scans occupied storage;
canonical_root_fragments(pos:CellPos,fragments:&[CellFragment])->Vec<CellFragment>
normalizes already-valid roots: same-material full-solid boxes merge across
complete faces, while clipped warped sources keep their geometry.
World::apply_root_edits(edits:&[RootEdit])->Result<Vec<ChunkKey>,String> validates
and compiles all replacements before changing any world state. Full-root single
cells normalize back to coarse storage. get()/cells() expose coarse storage;
use the root APIs for complete geometry. Saved coarse and detailed positions
must not overlap. Legacy coarse-only snapshots remain readable.
pub fn forward(yaw:f32,pitch:f32)->Vec3 with yaw0 forward (0,0,-1), positive
yaw turns right (+x), positive pitch looks up (+y).
pub fn spawn_point(index:u32)->Vec3; feet coordinates in safe arena positions.

## Simulation

InputFrame { pub seq:u32, pub forward:f32, pub strafe:f32, pub vertical:f32,
pub yaw:f32, pub pitch:f32, pub jump:bool, pub fire:bool, pub weapon:u8 }
derive Default, Copy. Axes [-1,1], weapon0 hitscan1 rocket. dt always server-owned.
Player { pub id:u64, pub name:String, pub position:Vec3, pub velocity:Vec3,
pub yaw:f32, pub pitch:f32, pub editing:bool, pub mode_request:u32, pub grounded:bool,
pub health:u16, pub score:i32, pub last_input:u32 }
Player::new(id:u64,name:String)->Self; Player::eye()->Vec3.
pub fn simulate_player(world:&World,player:&mut Player,input:&InputFrame,dt:f32).
Physics only; combat/mode changes handled by server. Set player.last_input=seq.
pub fn player_intersects_cell(player:&Player,pos:CellPos,cell:Cell)->bool.
pub fn ray_player(origin:Vec3,direction:Vec3,player:&Player,max_distance:f32)->Option<f32>.

WorldObject { pub id:u32, pub position:Vec3, pub kind:ObjectKind,
pub rotation:f32 }. ObjectKind enum (serde snake_case): Tree, Light, Spawn, WallLamp, HangingLamp, UtilityBox, Pipe, Rock. Props have no gameplay collision yet; lamp variants participate in dynamic lighting.
pub fn arena_objects()->Vec<WorldObject>; industrial light fixtures and spawns.
Objects are visual/decorative initially except Spawn used on respawn; trees
do not claim detailed collider support. Shared placement/removal in client.
Projectile { pub id:u64, pub owner:u64, pub position:Vec3, pub velocity:Vec3 }.
ShotEffect { pub from:Vec3, pub to:Vec3, pub owner:u64, pub hit:bool }.

## Wire protocol

ClientMessage variants:
Hello { name:String }
Input { input:InputFrame }
SetMode { request:u32, editing:bool }
Edit { request:u64, edits:Vec<CellEdit> }
EditRoots { request:u64, base_revision:u64, roots:Vec<RootEdit> }
PlaceObject { kind:ObjectKind, position:Vec3, rotation:f32 }
RemoveObject { id:u32 }
Undo
Redo
Save

ServerMessage variants:
ModeChanged { request:u32, editing:bool }
Welcome { id:u64, revision:u64, cells:Vec<CellEdit>, roots:Vec<RootEdit>,
          objects:Vec<WorldObject>,
          players:Vec<Player>, editing_allowed:bool }
Snapshot { tick:u64, revision:u64, players:Vec<Player>, projectiles:Vec<Projectile> }
WorldPatch { revision:u64, edits:Vec<CellEdit> }
RootPatch { revision:u64, roots:Vec<RootEdit> }
EditResult { request:u64, error:Option<String> }
Objects { revision:u64, objects:Vec<WorldObject> }
Shots { shots:Vec<ShotEffect> }
Notice { message:String }
Error { message:String }

Edits are bounded to16,384 changed roots and65,536 fragments, positions in bounds,
materials0..5, and valid heights0..8 with at least one nonzero or a validated
explicit Shape. EditRoots rejects replacements whose touched root changed after
base_revision; unrelated newer edits are allowed. This monotonic stale-edit check
is separate from the ownership lineage used for undo/redo.
Server rejects additions overlapping active fighters
and rejects edits unless editing. Per-player undo only reverts when all touched
cells still have the operation ownership token produced by that player's edit.
Undo restores the exact prior ownership lineage. Redo requires that lineage to
remain unchanged, then restores the original operation token, while the public
world revision still increases. New changed edits clear that player's redo
stack. History holds at most32 operations per player, further bounded by262,144
stored roots/fragments of complexity across before/after snapshots. Single room,
16 players max. Server starts localhost:3000 by default; optional explicit bind
argument/env for LAN. Save snapshots use cells, roots, objects and revision in JSON.
The roots field defaults to empty for old saves.
WebSocket transport is prototype bootstrap, no WAN responsiveness claim.
Incoming messages are capped at16MiB; outgoing write growth and saved-file
loading are capped at256MiB to account for explicit shapes. These are limits,
not preallocated connection buffers or validated practical world-size targets.
Each parsed Edit or EditRoots receives exactly one EditResult with the same request. Changed
edits broadcast WorldPatch or RootPatch before their successful result; unchanged edits also
succeed, without a patch. Failed edits return error:Some(message). Other actions
retain generic Error messages. Clients complete pending edits only from the
matching EditResult, never by matching geometry in another player's WorldPatch.

Each InputFrame describes exactly one fixed simulation step. Server consumes
commands in sequence, at most one per tick, and acknowledges only consumed seqs.
Queue capacity is64; overflow disconnects rather than silently skipping input.
Duplicate/stale seqs are ignored. Missing commands apply neutral movement while
gravity continues; last_input stays unchanged. Held fire expires after300ms
without a consumed command. SetMode drains the old-mode queue and acknowledges
its highest received seq, since clients discard their old-mode prediction.
ModeChanged is returned on success and denial with the actual editing flag;
Player.mode_request records the request in subsequent snapshots. Denial also
returns Error. Clients use mode_request to reject stale snapshot mode state.

Renderer: native/web wgpu24 + winit0.30 + egui0.31.
Core and server should remain independent of graphics dependencies.
