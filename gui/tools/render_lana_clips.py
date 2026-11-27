# render_lana_clips.py — Blender-side full-loop renderer for Lana's
# frozen reaction clips (torogao-motion-clip/2, the 32 band x tier
# clips of record in lana-reaction-animation-20261127) against the
# rung-1 rig. Driven by tools/make_lana_clips.py; not run directly.
#
#   blender -b -Y --offline-mode -noaudio -P render_lana_clips.py -- \
#       --blend <lana.blend> --clips-dir <frozen clip dir> \
#       --frames-dir <output dir> [--only <band.tN>] [--frames 0,36]
#
# Binding law: the rung-1 binder's (lana-rung1-20261127/build/
# bind_rung1.py), unchanged — joint quaternions are pose-bone local
# deltas; Hips translation is glTF-local bound as a delta from the
# clip-space rest (0, 0.98, 0); the original five expression carriers
# bind as deltas from the clip's frame-0 parameter state; the rung-1
# carriers (pupil dilate, eye gloss) bind absolute and linear, the
# patch carriers (tear sheen, blush, blush spread) absolute with the
# v^0.6 emergence gamma; skin_flush / skin_sheen bind onto the Skin
# material (MixRGB factor 0.55 x flush toward (0.72, 0.30, 0.24),
# roughness 0.85 - 0.5 x sheen). pupil_shape is asserted identically
# zero in every clip (canon: heart pupils are Prima/Alma's, never
# Lana's).
#
# Sampling law: the frozen format's own evaluation law — the one the
# torogao-runtime evaluator (clip_eval) applies and the clips' eval
# digests are computed over. Scalar segments marked cubic evaluate as
# cubic Hermite over the keys' stored Fritsch-Carlson tangents;
# linear segments lerp; step segments hold. Quaternion segments
# slerp. (The Blender *proof* binders sample scalars linearly, a
# disclosed proof-route simplification; playback frames reproduce the
# artefact as its own evaluator reads it. Pre-flight audit of this
# sampler over all 32 clips: zero out-of-key-range samples.)
#
# Scene: the band clips are solo standing loops — no set, no partner,
# no props (asserted: these clips carry none). Lighting is the
# binder's band rig (two suns, no world); film is transparent so she
# floats over the GUI stage exactly as the matted stills did. Eevee,
# 480x960 (2x the GUI figure's 240x480 display); the driver
# downscales into strip cells.
import bpy, json, math, os, sys, time
from mathutils import Vector, Quaternion

argv = sys.argv[sys.argv.index("--") + 1:]
def arg(name, default=None):
    return argv[argv.index(name) + 1] if name in argv else default

BLEND = arg("--blend")
CLIPS_DIR = arg("--clips-dir")
FRAMES_DIR = arg("--frames-dir")
ONLY = arg("--only")
FRAMES_SEL = arg("--frames")
os.makedirs(FRAMES_DIR, exist_ok=True)
bpy.ops.wm.open_mainfile(filepath=BLEND)
arm = next(o for o in bpy.data.objects if o.type == 'ARMATURE')
body = next(o for o in bpy.data.objects if o.type == 'MESH' and 'Body' in o.name)
outfit = next(o for o in bpy.data.objects if o.type == 'MESH' and o.name == 'Lana_Outfit')
for o in bpy.data.objects:
    if o.type == 'MESH':
        o.hide_render = ('OutfitEnd' in o.name)
        o.hide_viewport = o.hide_render
sc = bpy.context.scene
sc.render.engine = 'BLENDER_EEVEE'
sc.render.film_transparent = True
sc.render.resolution_x, sc.render.resolution_y = 480, 960
sc.render.resolution_percentage = 100
sc.render.image_settings.file_format = 'PNG'
sc.render.image_settings.color_mode = 'RGBA'
sc.render.image_settings.color_depth = '8'

# ---------------- clip sampling (the format's evaluation law) ----------------
def _hermite(v0, v1, m0, m1, dt, u):
    u2, u3 = u * u, u * u * u
    return ((2 * u3 - 3 * u2 + 1) * v0 + (u3 - 2 * u2 + u) * dt * m0
            + (-2 * u3 + 3 * u2) * v1 + (u3 - u2) * dt * m1)

def sample_keys(keys, t):
    if t <= keys[0]['frame']:
        return keys[0]['value']
    if t >= keys[-1]['frame']:
        return keys[-1]['value']
    for a, b in zip(keys, keys[1:]):
        if a['frame'] <= t <= b['frame']:
            if b['frame'] == a['frame']:
                return b['value']
            interp = b.get('interp', 'linear')
            if interp == 'step':
                return a['value']
            u = (t - a['frame']) / (b['frame'] - a['frame'])
            va, vb = a['value'], b['value']
            if isinstance(va, list) and len(va) == 4:
                qa = Quaternion((va[3], va[0], va[1], va[2]))
                qb = Quaternion((vb[3], vb[0], vb[1], vb[2]))
                q = qa.slerp(qb, u)
                return [q.x, q.y, q.z, q.w]
            if isinstance(va, list):
                return [x + (y - x) * u for x, y in zip(va, vb)]
            if interp == 'cubic':
                return _hermite(va, vb, a.get('out_tangent', 0.0),
                                b.get('in_tangent', 0.0),
                                b['frame'] - a['frame'], u)
            return va + (vb - va) * u
    return keys[-1]['value']

# ---------------- materials (skin_flush / skin_sheen carriers) ----------------
_SKIN_MIX = None

def set_skin(flush, sheen):
    global _SKIN_MIX
    skin = bpy.data.materials['Skin']
    nt = skin.node_tree
    bsdf = nt.nodes['Principled BSDF']
    if _SKIN_MIX is None:
        mix = nt.nodes.get('RungFlushMix')
        if mix is None:
            mix = nt.nodes.new('ShaderNodeMixRGB')
            mix.name = 'RungFlushMix'
            mix.blend_type = 'MIX'
            mix.inputs[2].default_value = (0.72, 0.30, 0.24, 1.0)
            mix.inputs[1].default_value = bsdf.inputs['Base Color'].default_value
            nt.links.new(mix.outputs[0], bsdf.inputs['Base Color'])
        _SKIN_MIX = mix
    _SKIN_MIX.inputs[0].default_value = 0.55 * flush
    bsdf.inputs['Roughness'].default_value = 0.85 - 0.5 * sheen

kb = body.data.shape_keys.key_blocks

def set_morph(name, val):
    if name in kb:
        kb[name].value = max(0.0, min(1.0, val))

# ---------------- scene: camera + the binder's band light rig ----------------
cam_d = bpy.data.cameras.new("clipcam")
cam_d.lens = 50
cam = bpy.data.objects.new("clipcam", cam_d)
sc.collection.objects.link(cam)
cam.location = (0, -2.75, 0.90)
cam.rotation_euler = (Vector((0, 0, 0.88)) - cam.location).to_track_quat('-Z', 'Y').to_euler()
sc.camera = cam
for nm, loc, rot, e in (("sun", (1.5, -2.0, 3.0), (math.radians(55), 0, math.radians(-25)), 3.0),
                        ("fill", (-2.0, -1.0, 1.5), (math.radians(70), 0, math.radians(140)), 1.2)):
    ld = bpy.data.lights.new(nm, 'SUN')
    ld.energy = e
    lo = bpy.data.objects.new(nm, ld)
    lo.location = loc
    lo.rotation_euler = rot
    sc.collection.objects.link(lo)

# ---------------- per-clip posing ----------------
def load_clip(path):
    clip = json.load(open(path))
    joint_ch, param_ch = {}, {}
    for ch in clip['channels']:
        t = ch['target']
        assert not (t['kind'] == 'joint' and (t['name'].startswith('partner:')
                    or t['name'].startswith('prop:'))), \
            f"{clip['name']}: band clips are solo; partner/prop channel found"
        if t['kind'] == 'joint':
            joint_ch[(t['name'], t['property'])] = ch['keys']
        else:
            param_ch[t['name']] = ch['keys']
    f0 = clip['range']['start']
    assert all(abs(sample_keys(param_ch['pupil_shape'], f)) < 1e-9
               for f in range(clip['range']['start'], clip['range']['end'] + 1)), \
        f"{clip['name']}: pupil_shape not identically 0"
    base = {n: sample_keys(k, f0) for n, k in param_ch.items()}
    return clip, joint_ch, param_ch, base

def apply_frame(joint_ch, param_ch, base, f):
    sc.frame_set(f)
    hips_keys = joint_ch.get(('mixamorig:Hips', 'translation'))
    rest = [0.0, 0.98, 0.0] if hips_keys else None
    Rinv = arm.pose.bones['mixamorig:Hips'].bone.matrix_local.to_3x3().inverted()
    for (bn, prop), keys in joint_ch.items():
        pb = arm.pose.bones[bn]
        v = sample_keys(keys, f)
        if prop == 'rotation':
            pb.rotation_mode = 'QUATERNION'
            pb.rotation_quaternion = Quaternion((v[3], v[0], v[1], v[2]))
        elif prop == 'translation' and bn == 'mixamorig:Hips':
            d = Vector((v[0] - rest[0], v[1] - rest[1], v[2] - rest[2]))
            pb.location = Rinv @ Vector((d.x, -d.z, d.y))
    if param_ch:
        P = {n: sample_keys(k, f) / 1000.0 for n, k in param_ch.items()}
        B = {n: v / 1000.0 for n, v in base.items()}
        da = P['eyelid_aperture'] - B['eyelid_aperture']
        set_morph('Fac_Eye_Close', -da if da < 0 else 0.0)
        set_morph('Fac_Eye_L_Open', da if da > 0 else 0.0)
        set_morph('Fac_Eye_R_Open', da if da > 0 else 0.0)
        dr = P['brow_raise'] - B['brow_raise']
        set_morph('Fac_Ebr_Up', dr if dr > 0 else 0.0)
        set_morph('Fac_Ebr_Relax', -dr if dr < 0 else 0.0)
        dg = P['brow_angle'] - B['brow_angle']
        set_morph('Fac_Ebr_Sad', dg if dg > 0 else 0.0)
        do = P['mouth_open'] - B['mouth_open']
        set_morph('Fac_Mth_Open', do if do > 0 else 0.0)
        ds = P['mouth_shape'] - B['mouth_shape']
        set_morph('Fac_Mth_Smile', 0.5 * ds if ds > 0 else 0.0)
        set_morph('Fac_Pupil_Heart', 0.0)
        if 'pupil_dilate' in P:
            set_morph('Fac_Pupil_Dilate', P['pupil_dilate'])
            set_morph('Fac_Eye_Gloss', P['eye_gloss'])
            set_morph('Fac_Tear_Sheen', P['tear_sheen'] ** 0.6)
            set_morph('Fac_Blush', P['blush_intensity'] ** 0.6)
            set_morph('Fac_Blush_Spread', P['blush_spread'] ** 0.6)
            set_skin(P['skin_flush'], P['skin_sheen'])
    bpy.context.view_layer.update()

# ---------------- render all clips ----------------
files = sorted(fn for fn in os.listdir(CLIPS_DIR) if fn.endswith('.clip.json'))
for fn in files:
    clip, joint_ch, param_ch, base = load_clip(os.path.join(CLIPS_DIR, fn))
    key = clip['name'].rsplit('/', 1)[-1]  # e.g. building.t1
    if ONLY and key != ONLY:
        continue
    F0, F1 = clip['range']['start'], clip['range']['end']
    out_dir = os.path.join(FRAMES_DIR, key)
    os.makedirs(out_dir, exist_ok=True)
    wanted = None
    if FRAMES_SEL:
        wanted = {int(x) for x in FRAMES_SEL.split(',')}
    t0 = time.time()
    n = 0
    for f in range(F0, F1):  # the end key duplicates frame start (loop closure)
        if wanted is not None and f not in wanted:
            continue
        apply_frame(joint_ch, param_ch, base, f)
        sc.render.filepath = os.path.join(out_dir, f"f{f - F0:04d}.png")
        bpy.ops.render.render(write_still=True)
        n += 1
    print(f"CLIP DONE {key} frames {n} secs {time.time() - t0:.1f}", flush=True)
print("RENDER_LANA_CLIPS DONE")
