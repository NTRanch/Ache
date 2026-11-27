#!/usr/bin/env python3
"""Estate-authored form-factor turntables for ache-gui.

Builds one stylised mesh per device family (lathe profiles + primitives;
NOT product likenesses — generic form-factor art in the app's plum/pink
palette), then renders a 12-frame turntable (30-degree steps) with
Cycles on CPU. Outputs, per family, into gui/assets/:
  families/<family>.png  frame 0 still (192x192, transparent)
  spin/<family>.png      12-frame strip (2304x192), Slint source-clip

Run with the official Blender build on primo:
  blender --background --python tools/make_turntables.py -- <gui_dir>
"""
import math
import sys
from pathlib import Path

import bpy
from mathutils import Vector

FRAMES = 12
SIZE = 192

BODY = (0.20, 0.05, 0.45, 1.0)      # plum purple
BODY2 = (0.36, 0.12, 0.66, 1.0)     # lighter violet
ACCENT = (0.93, 0.28, 0.60, 1.0)    # pink


def mat(name, rgba, rough=0.45):
    m = bpy.data.materials.new(name)
    m.use_nodes = True
    bsdf = m.node_tree.nodes["Principled BSDF"]
    bsdf.inputs["Base Color"].default_value = rgba
    bsdf.inputs["Roughness"].default_value = rough
    return m


MAT_BODY = mat("body", BODY)
MAT_BODY2 = mat("body2", BODY2, 0.35)
MAT_ACCENT = mat("accent", ACCENT, 0.3)


def revolve(profile, material, segments=48, z0=0.0):
    """Mesh from a (radius, z) profile revolved around Z."""
    verts, faces = [], []
    n = len(profile)
    for i, (r, z) in enumerate(profile):
        for j in range(segments):
            a = 2 * math.pi * j / segments
            verts.append((r * math.cos(a), r * math.sin(a), z + z0))
    for i in range(n - 1):
        for j in range(segments):
            j2 = (j + 1) % segments
            faces.append((i * segments + j, i * segments + j2,
                          (i + 1) * segments + j2, (i + 1) * segments + j))
    # cap the ends with a fan to the axis when radius > 0
    mesh = bpy.data.meshes.new("lathe")
    mesh.from_pydata(verts, [], faces)
    mesh.update()
    obj = bpy.data.objects.new("lathe", mesh)
    bpy.context.collection.objects.link(obj)
    obj.data.materials.append(material)
    return obj


def prim(kind, loc, scale, material, rot=(0, 0, 0)):
    if kind == "sphere":
        bpy.ops.mesh.primitive_uv_sphere_add(segments=32, ring_count=16, location=loc)
    elif kind == "cube":
        bpy.ops.mesh.primitive_cube_add(location=loc)
    elif kind == "cylinder":
        bpy.ops.mesh.primitive_cylinder_add(vertices=32, location=loc)
    elif kind == "torus":
        bpy.ops.mesh.primitive_torus_add(major_segments=40, minor_segments=12, location=loc)
    else:
        raise ValueError(kind)
    obj = bpy.context.object
    obj.scale = scale
    obj.rotation_euler = rot
    bpy.ops.object.transform_apply(scale=True)
    obj.data.materials.append(material)
    return obj


def button(parent_z=0.55, r=0.30):
    """Pink control dot proud of the body surface — reads in rotation."""
    return prim("sphere", (r, 0.0, parent_z), (0.055, 0.055, 0.055), MAT_ACCENT)


# --- family builders: each returns None; objects land in the scene ---

def b_egg():
    prim("sphere", (0, 0, 0.52), (0.30, 0.30, 0.44), MAT_BODY)
    prim("cylinder", (0, 0, 1.06), (0.035, 0.035, 0.22), MAT_BODY2)
    button(0.55, 0.29)


def b_insertable():
    revolve([(0.001, 0.0), (0.16, 0.0), (0.20, 0.25), (0.17, 0.62),
             (0.20, 0.88), (0.13, 1.05), (0.001, 1.12)], MAT_BODY)
    button(0.30, 0.185)


def b_rabbit():
    revolve([(0.001, 0.0), (0.17, 0.0), (0.21, 0.3), (0.18, 0.7),
             (0.14, 0.98), (0.001, 1.08)], MAT_BODY)
    prim("sphere", (0.20, 0, 0.60), (0.085, 0.085, 0.26), MAT_BODY2,
         rot=(0, math.radians(-30), 0))
    button(0.25, 0.20)


def b_plug():
    revolve([(0.001, 0.0), (0.30, 0.0), (0.31, 0.07), (0.10, 0.16),
             (0.11, 0.30), (0.24, 0.52), (0.26, 0.68), (0.16, 0.86),
             (0.001, 0.92)], MAT_BODY)
    prim("sphere", (0, 0, 0.035), (0.10, 0.10, 0.03), MAT_ACCENT)


def b_panty():
    prim("sphere", (0, 0, 0.42), (0.42, 0.30, 0.20), MAT_BODY)
    button(0.45, 0.40)


def b_suction():
    revolve([(0.001, 0.0), (0.15, 0.0), (0.17, 0.3), (0.16, 0.55),
             (0.24, 0.72), (0.30, 0.78), (0.30, 0.86), (0.22, 0.86),
             (0.20, 0.78), (0.13, 0.66), (0.12, 0.4)], MAT_BODY)
    button(0.35, 0.165)


def b_butterfly():
    prim("sphere", (0, 0, 0.45), (0.16, 0.14, 0.34), MAT_BODY)
    prim("sphere", (0.26, 0, 0.52), (0.24, 0.10, 0.16), MAT_BODY2,
         rot=(0, 0, math.radians(18)))
    prim("sphere", (-0.26, 0, 0.52), (0.24, 0.10, 0.16), MAT_BODY2,
         rot=(0, 0, math.radians(-18)))
    button(0.45, 0.15)


def b_kegel():
    prim("sphere", (0, 0, 0.30), (0.21, 0.21, 0.24), MAT_BODY)
    prim("sphere", (0, 0, 0.68), (0.17, 0.17, 0.20), MAT_BODY2)
    prim("cylinder", (0.10, 0, 0.94), (0.02, 0.02, 0.16), MAT_ACCENT)


def b_bullet():
    prim("cylinder", (0, 0, 0.38), (0.13, 0.13, 0.30), MAT_BODY)
    prim("sphere", (0, 0, 0.68), (0.13, 0.13, 0.13), MAT_BODY)
    prim("sphere", (0, 0, 0.08), (0.13, 0.13, 0.08), MAT_BODY)
    button(0.42, 0.125)


def b_wand():
    prim("cylinder", (0, 0, 0.30), (0.11, 0.11, 0.30), MAT_BODY2)
    prim("sphere", (0, 0, 0.78), (0.24, 0.24, 0.26), MAT_BODY)
    button(0.32, 0.105)


def b_clip():
    prim("cylinder", (0, 0, 0.34), (0.10, 0.10, 0.24), MAT_BODY)
    prim("sphere", (0, 0, 0.58), (0.10, 0.10, 0.10), MAT_BODY)
    prim("cube", (0.13, 0, 0.20), (0.05, 0.09, 0.16), MAT_ACCENT)


def b_external():
    prim("sphere", (0, 0, 0.38), (0.34, 0.26, 0.22), MAT_BODY)
    button(0.42, 0.30)


def b_rideon():
    prim("cube", (0, 0, 0.18), (0.44, 0.34, 0.18), MAT_BODY2)
    prim("sphere", (0, 0, 0.40), (0.30, 0.26, 0.16), MAT_BODY)
    prim("cylinder", (0, 0, 0.52), (0.05, 0.05, 0.10), MAT_ACCENT)


def b_nipple():
    prim("sphere", (0.16, 0, 0.30), (0.17, 0.17, 0.20), MAT_BODY)
    prim("sphere", (-0.16, 0, 0.30), (0.17, 0.17, 0.20), MAT_BODY)
    prim("cylinder", (0, 0, 0.34), (0.30, 0.03, 0.03), MAT_ACCENT,
         rot=(0, math.radians(90), 0))


def b_beads():
    for i, (r, z) in enumerate([(0.10, 0.16), (0.13, 0.38), (0.16, 0.64),
                                (0.19, 0.94)]):
        prim("sphere", (0, 0, z), (r, r, r * 1.15), MAT_BODY if i % 2 == 0 else MAT_BODY2)
    prim("torus", (0, 0, 0.05), (1, 1, 1), MAT_ACCENT)


def b_controller():
    prim("cube", (0, 0, 0.30), (0.30, 0.20, 0.30), MAT_BODY2)
    button(0.38, 0.0)
    prim("sphere", (0.0, 0.21, 0.38), (0.06, 0.03, 0.06), MAT_ACCENT)


def b_other():
    prim("sphere", (0, 0, 0.42), (0.30, 0.24, 0.30), MAT_BODY2)
    button(0.45, 0.27)


BUILDERS = {
    "egg": b_egg, "insertable": b_insertable, "rabbit": b_rabbit,
    "plug": b_plug, "panty": b_panty, "suction": b_suction,
    "butterfly": b_butterfly, "kegel": b_kegel, "bullet": b_bullet,
    "wand": b_wand, "clip": b_clip, "external": b_external,
    "rideon": b_rideon, "nipple": b_nipple, "beads": b_beads,
    "controller": b_controller, "other": b_other,
}


def setup_scene():
    bpy.ops.wm.read_factory_settings(use_empty=True)
    scene = bpy.context.scene
    scene.render.engine = "CYCLES"
    scene.cycles.samples = 24
    scene.view_settings.view_transform = "Standard"
    scene.cycles.use_denoising = False
    scene.render.resolution_x = SIZE
    scene.render.resolution_y = SIZE
    scene.render.film_transparent = True
    scene.render.image_settings.file_format = "PNG"
    scene.render.image_settings.color_mode = "RGBA"
    # camera
    cam_data = bpy.data.cameras.new("cam")
    cam = bpy.data.objects.new("cam", cam_data)
    bpy.context.collection.objects.link(cam)
    cam.location = (0.0, -3.3, 1.30)
    cam.rotation_euler = (math.radians(66), 0, 0)
    scene.camera = cam
    # lights: key + rim, no world light (transparent film)
    for name, loc, energy, size in [("key", (1.6, -1.8, 2.6), 240, 2.0),
                                    ("rim", (-1.8, 1.6, 1.8), 170, 2.0),
                                    ("fill", (-1.2, -2.0, 0.8), 90, 2.5)]:
        ld = bpy.data.lights.new(name, "AREA")
        ld.energy = energy
        ld.shape = "DISK"
        ld.size = size
        lo = bpy.data.objects.new(name, ld)
        bpy.context.collection.objects.link(lo)
        lo.location = loc
        lo.rotation_euler = (math.radians(50), 0, math.radians(30))
    return scene


def main():
    argv = sys.argv[sys.argv.index("--") + 1:]
    gui_dir = Path(argv[0])
    fam_dir = gui_dir / "assets" / "families"
    spin_dir = gui_dir / "assets" / "spin"
    tmp_dir = gui_dir / "assets" / "frames"
    fam_dir.mkdir(parents=True, exist_ok=True)
    spin_dir.mkdir(parents=True, exist_ok=True)
    tmp_dir.mkdir(parents=True, exist_ok=True)

    only = argv[1].split(",") if len(argv) > 1 and argv[1] else None
    for family, builder in BUILDERS.items():
        if only and family not in only:
            continue
        scene = setup_scene()
        # materials must be (re)created per fresh scene
        global MAT_BODY, MAT_BODY2, MAT_ACCENT
        MAT_BODY = mat("body", BODY)
        MAT_BODY2 = mat("body2", BODY2, 0.35)
        MAT_ACCENT = mat("accent", ACCENT, 0.3)
        builder()
        group = bpy.data.objects.new("turntable", None)
        bpy.context.collection.objects.link(group)
        for obj in list(bpy.context.collection.objects):
            if obj.type == "MESH":
                obj.parent = group
        frames = []
        for i in range(FRAMES):
            group.rotation_euler = (0, 0, math.radians(30 * i))
            out = tmp_dir / f"{family}-{i:02d}.png"
            scene.render.filepath = str(out)
            bpy.ops.render.render(write_still=True)
            frames.append(out)
        print(f"family {family}: {len(frames)} frames rendered")
    print("ALL FAMILIES DONE")


main()
