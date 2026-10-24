#!/usr/bin/env python3
"""Prepare Sauerteig's five static CC0 props; never needed for ordinary builds.

python3 -m venv .local/prop-tools/venv
.local/prop-tools/venv/bin/pip install pillow==13.0.0 numpy==2.5.4
npm install --prefix .local/prop-tools meshoptimizer@1.0.1 --ignore-scripts
.local/prop-tools/venv/bin/python scripts/prepare-props.py [--offline]

Powered by Poly Haven (https://polyhaven.com); source assets are CC0.
API terms: https://github.com/Poly-Haven/Public-API/blob/master/ToS.md
"""
import argparse, hashlib, json, pathlib, struct, subprocess, time, urllib.request
import numpy as np
from PIL import Image

ROOT = pathlib.Path(__file__).resolve().parent.parent
CACHE = ROOT / '.local/prop-sources'
OUT = ROOT / 'assets/props'
HEADERS = {'User-Agent': 'Sauerteig-asset-preparation/0.1 (https://github.com/zachmeador/sauerteig)'}
# key, source id, selected node, scale, near triangles, far triangles, material ids
SPECS = [
 ('wall_lamp','industrial_wall_lamp',0,1.,2600,700,[8,9]),
 ('hanging_lamp','hanging_industrial_lamp',0,1.,3000,850,[10,11]),
 ('utility_box','utility_box_01',0,1.,2000,500,[12]),
 ('pipe','modular_industrial_pipes_01',1,1.,648,220,[13]),
 ('rock','rock_07',0,8.,1800,400,[14]),
]

def read(url):
 for attempt in range(4):
  try:
   with urllib.request.urlopen(urllib.request.Request(url,headers=HEADERS),timeout=60) as r:
    return r.read()
  except Exception:
   if attempt==3: raise
   time.sleep(attempt+1)

def fetch(asset,offline):
 path=CACHE/asset;path.mkdir(parents=True,exist_ok=True)
 for ep in ('files','info'):
  p=path/(ep+'.json')
  if not p.exists():
   if offline: raise ValueError(f'missing cached {p}')
   p.write_bytes(read(f'https://api.polyhaven.com/{ep}/{asset}'))
 spec=json.loads((path/'files.json').read_text())['gltf']['1k']['gltf']
 downloads={asset+'.gltf':spec,**spec['include']}
 provenance=[]
 for name,f in downloads.items():
  p=path/name
  if path not in p.resolve().parents: raise ValueError('unsafe source filename')
  p.parent.mkdir(parents=True,exist_ok=True)
  if not p.exists():
   if offline: raise ValueError(f'missing cached {p}')
   p.write_bytes(read(f['url']))
  data=p.read_bytes()
  if hashlib.md5(data).hexdigest()!=f['md5']: raise ValueError(f'source checksum mismatch: {p}')
  provenance.append({'file':name,'url':f['url'],'sha256':hashlib.sha256(data).hexdigest(),'bytes':len(data)})
 return path,json.loads((path/(asset+'.gltf')).read_text()),provenance

def accessor(doc,path,index):
 a=doc['accessors'][index];view=doc['bufferViews'][a['bufferView']]
 if a.get('sparse') or a.get('normalized'): raise ValueError('unexpected glTF accessor')
 components={'SCALAR':1,'VEC2':2,'VEC3':3,'VEC4':4}[a['type']]
 dtype=np.dtype({5121:'u1',5123:'<u2',5125:'<u4',5126:'<f4'}[a['componentType']])
 buffer=(path/doc['buffers'][view['buffer']]['uri']).read_bytes()
 return np.ndarray((a['count'],components),dtype=dtype,buffer=buffer,
  offset=view.get('byteOffset',0)+a.get('byteOffset',0),
  strides=(view.get('byteStride',dtype.itemsize*components),dtype.itemsize)).copy()

def texture(doc,path,ref,default):
 if ref is None: return Image.new('RGB',(512,512),default)
 image=doc['images'][doc['textures'][ref['index']]['source']]
 return Image.open(path/image['uri']).convert('RGB').resize((512,512),Image.Resampling.LANCZOS)

def cook_texture(doc,path,mat_id,slot,assetkey):
 mat=doc['materials'][mat_id];pbr=mat.get('pbrMetallicRoughness',{})
 color=texture(doc,path,pbr.get('baseColorTexture'),(255,255,255))
 rgb=np.asarray(color,dtype=np.float32)/255
 factor=np.array(pbr.get('baseColorFactor',[1,1,1,1])[:3])
 rgb=np.clip(rgb*factor,0,1)
 arm=texture(doc,path,pbr.get('metallicRoughnessTexture'),(255,255,255))
 channels=np.asarray(arm)
 roughness=np.asarray(channels[:,:,1],dtype=float)*pbr.get('roughnessFactor',1)
 metallic=float(channels[:,:,2].mean()/255*pbr.get('metallicFactor',1))
 normal=np.asarray(texture(doc,path,mat.get('normalTexture'),(128,128,255))).copy()
 # Normalize after resampling. Glass is deliberately opaque, warm frosted glass.
 n=normal.astype(float)/127.5-1;n/=np.maximum(np.linalg.norm(n,axis=2,keepdims=True),1e-8)
 normal=np.clip(np.rint((n+1)*127.5),0,255).astype('uint8')
 glass='glass' in mat.get('name','')
 if glass:
  rgb=np.clip(rgb*np.array([1.,.88,.67])*.7+np.array([.30,.23,.12]),0,1)
  roughness=np.full((512,512),170)
 rgba=np.empty((512,512,4),dtype='uint8');rgba[:,:,:3]=np.rint(rgb*255).astype('uint8');rgba[:,:,3]=channels[:,:,0]
 nr=np.empty((512,512,4),dtype='uint8');nr[:,:,:3]=normal;nr[:,:,3]=np.clip(np.rint(roughness),0,255).astype('uint8')
 name=f'{slot:02d}_{assetkey}'+('_glass' if glass else '')
 folder=OUT/name;folder.mkdir(parents=True,exist_ok=True)
 Image.fromarray(rgba).save(folder/'albedo.png',optimize=True)
 Image.fromarray(nr).save(folder/'normal_roughness.png',optimize=True)
 return {'slot':slot,'key':name,'source_material':mat.get('name'), 'metallic':round(metallic,4),'emission':.8 if glass else 0.0}

def simplify(vertices,indices,target,error):
 input=json.dumps({'vertices':vertices.tolist(),'indices':indices.tolist(),'target':target,'error':error})
 return json.loads(subprocess.run(['node',str(ROOT/'scripts/simplify-prop.mjs')],input=input,text=True,capture_output=True,check=True).stdout)

def write_mesh(path,parts):
 vertices=[];indices=[]
 for p,slot in parts:
  offset=len(vertices)
  vertices.extend((v,slot) for v in p['vertices']);indices.extend(i+offset for i in p['indices'])
 result=bytearray(struct.pack('<4sII',b'SPM1',len(vertices),len(indices)))
 for v,slot in vertices:result.extend(struct.pack('<8fIf',*v,slot,0.))
 result.extend(struct.pack('<'+'I'*len(indices),*indices));path.write_bytes(result)
 return {'vertices':len(vertices),'triangles':len(indices)//3,'bytes':len(result),'sha256':hashlib.sha256(result).hexdigest()}

def main():
 args=argparse.ArgumentParser();args.add_argument('--offline',action='store_true');args=args.parse_args()
 OUT.mkdir(parents=True,exist_ok=True);assets=[];textures=[]
 for key,asset,node_index,scale,near_target,far_target,slots in SPECS:
  path,doc,sourcefiles=fetch(asset,args.offline)
  node=doc['nodes'][node_index]
  if any(k in node for k in ('matrix','rotation','scale','children')):raise ValueError('unexpected node transform')
  # Kit layout translations are intentionally removed: each extracted prop has its own pivot.
  mesh=doc['meshes'][node['mesh']];parts=[];near=[];far=[]
  total=sum(doc['accessors'][p['indices']]['count']//3 for p in mesh['primitives'])
  for primitive,slot in zip(mesh['primitives'],slots,strict=True):
   if primitive.get('mode',4)!=4:raise ValueError('only triangle lists supported')
   attrs=primitive['attributes'];p=accessor(doc,path,attrs['POSITION'])*scale
   n=accessor(doc,path,attrs['NORMAL']);uv=accessor(doc,path,attrs['TEXCOORD_0'])
   # Floor-standing props get ground-center pivots; lamps retain authored mounting pivots.
   if key in ('utility_box','pipe','rock'):
    p[:,0]-=(p[:,0].min()+p[:,0].max())/2;p[:,1]-=p[:,1].min();p[:,2]-=(p[:,2].min()+p[:,2].max())/2
   vertices=np.concatenate((p,n,uv),axis=1);indices=accessor(doc,path,primitive['indices']).flatten()
   if not np.isfinite(vertices).all():raise ValueError('nonfinite vertices')
   proportion=(len(indices)//3)/total
   near.append((simplify(vertices,indices,max(12,int(near_target*proportion)),.006),slot))
   far.append((simplify(vertices,indices,max(12,int(far_target*proportion)),.035),slot))
   parts.append(vertices)
   textures.append(cook_texture(doc,path,primitive['material'],slot,key))
  allvertices=np.concatenate(parts);info=json.loads((path/'info.json').read_text())
  assets.append({'key':key,'source':asset,'url':f'https://polyhaven.com/a/{asset}',
   'authors':info.get('authors'),'license':'CC0-1.0','selected_node':node['name'],'scale':scale,
   'source_triangles':total,'bounds_min':allvertices[:,:3].min(axis=0).tolist(),'bounds_max':allvertices[:,:3].max(axis=0).tolist(),
   'near':write_mesh(OUT/(key+'.spm'),near),'distant':write_mesh(OUT/(key+'_distant.spm'),far),
   'source_files':sourcefiles})
  print(key,assets[-1]['near']['triangles'],assets[-1]['distant']['triangles'],flush=True)
 manifest={'schema':1,'license_url':'https://polyhaven.com/license','texture_size':512,'texture_packing':{'albedo':'sRGB RGB + linear ambient occlusion alpha','normal_roughness':'OpenGL tangent-space XYZ encoded RGB + linear roughness alpha'},'assets':assets,'textures':textures}
 (OUT/'manifest.json').write_text(json.dumps(manifest,indent=2)+'\n')
if __name__=='__main__':main()
