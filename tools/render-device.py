"""Render the device images used by the README with Blender (Cycles, CPU).

Two views: `front` from almost straight above, `quarter` from forty degrees. Run headless:

    blender -b -P tools/render-device.py -- --out target/render

Everything after `--` belongs to this script; Blender ignores it.

    --out DIR      where the PNGs go (default target/render)
    --views LIST   comma separated: front, quarter (default: both)
    --dark         also render the dark variant of every view
    --quick        800 px and 24 samples, for looking at the geometry
    --samples N    override the sample count
    --transparent  no backdrop: alpha instead, with the contact shadow kept
    --ground STYLE wood (default) or grey, for the surface the device stands on
    --width N      override the long edge in pixels
    --art DIR      screen images per view (default media/screen)
"""

import math
import os
import sys

import bmesh
import bpy
from mathutils import Vector

# --- geometry, millimetres ----------------------------------------------------
# Diameter and height are measured; glass and active area are ratios of the outer radius taken
# from a frontal photograph (the active area comes out at the 1.8-inch panel's 45.7 mm).
KNOB_D = 66.0          # outer diameter of the red knob ring, measured
BASE_D = 66.0          # the base is the same width, measured
TOTAL_H = 22.0         # overall height, measured

# How the height divides, measured from side photographs.
KNOB_H = 13.4
GAP_H = 0.7            # the shadow line between the turning part and the base
BASE_H = TOTAL_H - KNOB_H - GAP_H
FOOT_H = 1.1           # the base stands on a narrower foot
FOOT_D = 62.5

GLASS_D = KNOB_D * 0.839   # black glass, edge to edge
ACTIVE_D = KNOB_D * 0.696  # the 360x360 pixels the firmware draws into
# The knob's face is a shallow cone falling towards the glass, which lies at its inner edge.
FACE_DISH = 0.7            # how far the inner edge of the face lies below the outer one
GLASS_FILLET = 0.5         # radius of the rounded edge of the glass
# The flutes, counted and measured on side photographs: flat facets between sharp crests,
# slightly hollow, climbing at a constant angle.
FLUTES = 29
FLUTE_SLANT = 40.0         # degrees from the axis
FLUTE_SAG = 0.3            # how far the middle of a facet lies below its chord
FLUTE_POINTS = 6           # vertices across one flute
KNOB_BEVEL = 0.8           # chamfer on the knob's top edge, the only smooth part of it
BASE_BEVEL = 1.0

# The openings in the base, measured from photographs of its side. Azimuth in degrees (-90 is
# the -y side), width along the circumference and height in millimetres.
PORTS = {
    "mic": dict(azimuth=-111, w=1.1, h=1.1),
    "usb": dict(azimuth=-90, w=9.4, h=3.5),
    "jack": dict(azimuth=-57, w=4.0, h=4.0),
    "switch": dict(azimuth=-34, w=6.5, h=3.1),
}
PORT_DEPTH = 1.2
SWITCH_PROUD = 1.0         # the switch lever stands out of the base

# --- colours ------------------------------------------------------------------
RED_ANODISED = (0.74, 0.105, 0.085)   # linear; matt anodised aluminium, the turning part
BASE_BLACK = (0.022, 0.022, 0.024)    # the base below the seam
GLASS_BLACK = (0.004, 0.004, 0.005)
NEON_BLUE = (0.0, 0.42, 1.0)          # the openings glow, or they are black on black
NEON_STRENGTH = 8.0
GLOW_THRESHOLD = 6.5                  # above the screen's brightest pixels, so it stays sharp
GROUND_LIGHT = (0.78, 0.78, 0.79)
GROUND_DARK = (0.045, 0.046, 0.050)
WOOD_LIGHT = (0.700, 0.455, 0.215)    # linear; light brown, the wood between the grain
WOOD_DARK = (0.420, 0.240, 0.090)     # the grain itself

SCREEN_ART = {                        # what each view shows on the glass
    "front": "home.png",
    "quarter": "player.png",
}


def argv():
    """Our own arguments: everything Blender left after `--`."""
    return sys.argv[sys.argv.index("--") + 1:] if "--" in sys.argv else []


def flag(name, default=None, store=False):
    args = argv()
    if name not in args:
        return False if store else default
    if store:
        return True
    i = args.index(name)
    return args[i + 1] if i + 1 < len(args) else default


# --- scene helpers ------------------------------------------------------------

def clear():
    bpy.ops.wm.read_factory_settings(use_empty=True)


def material(name, colour, roughness=0.5, metallic=0.0, emission=None, strength=1.0):
    mat = bpy.data.materials.new(name)
    mat.use_nodes = True
    bsdf = mat.node_tree.nodes["Principled BSDF"]
    bsdf.inputs["Base Color"].default_value = (*colour, 1.0)
    bsdf.inputs["Roughness"].default_value = roughness
    bsdf.inputs["Metallic"].default_value = metallic
    if emission is not None:
        bsdf.inputs["Emission Color"].default_value = (*emission, 1.0)
        bsdf.inputs["Emission Strength"].default_value = strength
    return mat


def screen_material(path):
    """The glass shows an image, and it shows it by glowing, not by reflecting."""
    mat = bpy.data.materials.new("screen")
    mat.use_nodes = True
    nodes, links = mat.node_tree.nodes, mat.node_tree.links
    bsdf = nodes["Principled BSDF"]
    bsdf.inputs["Base Color"].default_value = (0, 0, 0, 1)
    bsdf.inputs["Roughness"].default_value = 0.12
    if path and os.path.exists(path):
        tex = nodes.new("ShaderNodeTexImage")
        tex.image = bpy.data.images.load(path)
        tex.interpolation = "Closest"     # the panel has pixels, so show pixels
        tex.location = (-400, 0)
        links.new(tex.outputs["Color"], bsdf.inputs["Emission Color"])
        # Below about 5 the glass's reflections wash the picture out.
        bsdf.inputs["Emission Strength"].default_value = 5.5
    else:
        bsdf.inputs["Emission Color"].default_value = (0.02, 0.02, 0.025, 1)
        bsdf.inputs["Emission Strength"].default_value = 1.0
    return mat


def cylinder(name, radius, depth, z, verts=128, bevel=0.0, mat=None):
    bpy.ops.mesh.primitive_cylinder_add(vertices=verts, radius=radius, depth=depth,
                                        location=(0, 0, z))
    ob = bpy.context.object
    ob.name = name
    if bevel:
        m = ob.modifiers.new("bevel", "BEVEL")
        m.width, m.segments, m.limit_method = bevel, 6, "ANGLE"
    # Smooth around the barrel, flat across the top: the sharp rim is the shape.
    if hasattr(bpy.ops.object, "shade_auto_smooth"):
        bpy.ops.object.shade_auto_smooth(angle=math.radians(30))
    else:
        bpy.ops.object.shade_smooth()
    if mat:
        ob.data.materials.append(mat)
    return ob


def knurled_knob(name, radius, height, bottom, teeth=FLUTES, sag=FLUTE_SAG,
                 bevel=0.8, slant=FLUTE_SLANT, dish=0.0, bore=None, mat=None):
    """The whole turning part as one mesh, its profile written out as rings:

        bottom ......... full flutes
        chamfer ........ a quarter circle in to the face, flutes dying out across it
        face ........... a shallow cone falling `dish` towards the glass
        bore ........... straight down under the glass, so its rounded edge has a groove

    A single mesh avoids a bevel modifier, which would also round the lower rim and leave a
    visible groove. The twist per millimetre is constant, so flutes climb at `slant` degrees (a
    diagonal knurl). Each flute is a facet between two crests, hollowed by `sag`.
    """
    spokes = teeth * FLUTE_POINTS
    step = 2 * math.pi / spokes

    # (height above the bottom, how much of the flute depth is left, radius of the ring)
    # Flutes run the full wall height and fade out across the chamfer, not at its foot.
    rings = [(0.0, 1.0, radius), (height - bevel, 1.0, radius)]
    arc = 5
    for i in range(1, arc + 1):
        a = (i / arc) * (math.pi / 2)
        depth = math.cos(a) if i < arc else 0.0
        rings.append((height - bevel + bevel * math.sin(a), depth,
                      radius - bevel * (1 - math.cos(a))))
    knurled = len(rings)
    # Rings past the rim have no flutes and keep the rim's twist, so the face gets straight
    # spokes rather than a pinwheel.
    if bore:
        rings.append((height - dish, 0.0, bore))
        rings.append((height - dish - 2.0, 0.0, bore))

    verts = []
    for n, (zt, depth, ring_r) in enumerate(rings):
        zt_twist = zt if n < knurled else height
        twist = math.tan(math.radians(slant)) * zt_twist / radius if slant else 0.0
        for i in range(spokes):
            # On the chord between two crests, less the hollow; `depth` fades both towards the face.
            u = (i % FLUTE_POINTS) / FLUTE_POINTS
            chord = math.cos(math.pi / teeth) / math.cos((u - 0.5) * 2 * math.pi / teeth)
            r = ring_r * (1 - depth * (1 - chord)) - sag * depth * 4 * u * (1 - u)
            theta = i * step + twist
            verts.append((r * math.cos(theta), r * math.sin(theta), zt - height / 2))

    faces = []
    for ring in range(len(rings) - 1):
        a, b = ring * spokes, (ring + 1) * spokes
        for i in range(spokes):
            j = (i + 1) % spokes
            faces.append((a + i, a + j, b + j, b + i))
    faces.append(tuple(range(spokes - 1, -1, -1)))                       # bottom
    faces.append(tuple(range(len(verts) - spokes, len(verts))))          # top face

    mesh = bpy.data.meshes.new(name)
    mesh.from_pydata(verts, [], faces)
    mesh.update()
    ob = bpy.data.objects.new(name, mesh)
    bpy.context.collection.objects.link(ob)
    ob.location = (0, 0, bottom + height / 2)

    # Mark the crests sharp and leave the facets smooth: an angle threshold would either facet
    # the chamfer or melt the crests. On the face they would facet it into wedges.
    for edge in mesh.edges:
        a, b = edge.vertices
        if (a % spokes == b % spokes and a % FLUTE_POINTS == 0
                and max(a, b) < knurled * spokes):
            edge.use_edge_sharp = True

    bpy.ops.object.select_all(action="DESELECT")
    ob.select_set(True)
    bpy.context.view_layer.objects.active = ob
    bpy.ops.object.shade_smooth()
    if mat:
        ob.data.materials.append(mat)
    return ob


def filleted_glass(name, radius, top, fillet, depth=2.5, verts=256, mat=None, edge_mat=None):
    """The glass as a turned profile: a straight wall, then a quarter circle into the top."""
    profile = [(top - depth, radius)]
    arc = 8
    for i in range(arc + 1):
        a = (i / arc) * (math.pi / 2)
        profile.append((top - fillet + fillet * math.sin(a),
                        radius - fillet * (1 - math.cos(a))))
    step = 2 * math.pi / verts
    coords = [(r * math.cos(i * step), r * math.sin(i * step), z)
              for z, r in profile for i in range(verts)]
    faces = []
    for ring in range(len(profile) - 1):
        a, b = ring * verts, (ring + 1) * verts
        for i in range(verts):
            j = (i + 1) % verts
            faces.append((a + i, a + j, b + j, b + i))
    faces.append(tuple(range(verts - 1, -1, -1)))
    faces.append(tuple(range(len(coords) - verts, len(coords))))
    mesh = bpy.data.meshes.new(name)
    mesh.from_pydata(coords, [], faces)
    mesh.update()
    ob = bpy.data.objects.new(name, mesh)
    bpy.context.collection.objects.link(ob)
    bpy.ops.object.select_all(action="DESELECT")
    ob.select_set(True)
    bpy.context.view_layer.objects.active = ob
    bpy.ops.object.shade_smooth()
    # The flat top stays flat: without this the smoothing bends its normals into the fillet.
    for edge in mesh.edges:
        a, b = edge.vertices
        if a >= len(coords) - verts and b >= len(coords) - verts:
            edge.use_edge_sharp = True
    if mat:
        mesh.materials.append(mat)
    # The fillet gets a glossier material; with the near-matt glass it vanishes into the black.
    if mat and edge_mat:
        mesh.materials.append(edge_mat)
        for poly in mesh.polygons[verts:verts * arc + verts]:
            poly.material_index = 1
    return ob


def boolean_difference(target, cutter):
    """Cut `cutter` out of `target`, applying the target's modifiers first.

    A boolean applied below an unapplied bevel leaves a shattered, faceted rim.
    """
    bpy.context.view_layer.objects.active = target
    for existing in list(target.modifiers):
        bpy.ops.object.modifier_apply(modifier=existing.name)
    m = target.modifiers.new("cut", "BOOLEAN")
    m.operation, m.object, m.solver = "DIFFERENCE", cutter, "EXACT"
    m.material_mode = "TRANSFER"
    bpy.context.view_layer.objects.active = target
    bpy.ops.object.modifier_apply(modifier=m.name)
    bpy.data.objects.remove(cutter, do_unlink=True)


def build_device(art_path):
    red = material("anodised red", RED_ANODISED, roughness=0.34, metallic=0.85)
    # Almost no specular, or the glass mirrors the red rim and the black bezel turns maroon.
    black = material("glass black", GLASS_BLACK, roughness=0.72, metallic=0.0)
    black.node_tree.nodes["Principled BSDF"].inputs["Specular IOR Level"].default_value = 0.04
    dark = material("socket", (0.02, 0.02, 0.02), roughness=0.6)

    base_black = material("base black", BASE_BLACK, roughness=0.52, metallic=0.35)
    cylinder("foot", FOOT_D / 2, FOOT_H + 0.2, (FOOT_H + 0.2) / 2, bevel=0.3, mat=base_black)
    cylinder("base", BASE_D / 2, BASE_H - FOOT_H, (FOOT_H + BASE_H) / 2, bevel=BASE_BEVEL,
             mat=base_black)

    top = BASE_H + GAP_H + KNOB_H
    knurled_knob("knob", KNOB_D / 2, KNOB_H, BASE_H + GAP_H, bevel=KNOB_BEVEL,
                 dish=FACE_DISH, bore=GLASS_D / 2, mat=red)

    # The glass sits in the bore of the knob's profile (a boolean recess leaves a faceted red
    # band), level with the face's inner edge and slightly inside the bore to avoid z-fighting.
    glass_top = top - FACE_DISH
    edge = material("glass edge", GLASS_BLACK, roughness=0.22, metallic=0.0)
    filleted_glass("glass", GLASS_D / 2 - 0.05, glass_top, GLASS_FILLET, mat=black, edge_mat=edge)

    screen = cylinder("screen", ACTIVE_D / 2, 0.05, glass_top + 0.05, mat=None)
    screen.data.materials.append(screen_material(art_path))
    unwrap_disc(screen)

    ports(dark)


def stadium(w, h, n=48):
    """Outline of a slot with round ends, as (u, v) round its centre; w == h gives a circle."""
    r, half = h / 2, (w - h) / 2
    pts = []
    for i in range(n):
        a = -math.pi / 2 + 2 * math.pi * i / n
        c = half if math.cos(a) >= 0 else -half
        pts.append((c + r * math.cos(a), r * math.sin(a)))
    return pts


def rect(w, h, cu=0.0):
    return [(cu - w / 2, -h / 2), (cu + w / 2, -h / 2), (cu + w / 2, h / 2), (cu - w / 2, h / 2)]


def prism(name, outline, y0, y1, azimuth, z, mat):
    """Extrude an outline radially from y0 to y1 (0 is the base's surface) at `azimuth`."""
    bm = bmesh.new()
    rings = [[bm.verts.new((u, y, v)) for u, v in outline] for y in (y0, y1)]
    bm.faces.new(rings[0])
    bm.faces.new(rings[1])
    n = len(outline)
    for i in range(n):
        j = (i + 1) % n
        bm.faces.new((rings[0][i], rings[0][j], rings[1][j], rings[1][i]))
    bmesh.ops.recalc_face_normals(bm, faces=bm.faces)
    mesh = bpy.data.meshes.new(name)
    bm.to_mesh(mesh)
    bm.free()
    mesh.materials.append(mat)
    ob = bpy.data.objects.new(name, mesh)
    bpy.context.collection.objects.link(ob)
    a = math.radians(azimuth)
    ob.location = (BASE_D / 2 * math.cos(a), BASE_D / 2 * math.sin(a), z)
    ob.rotation_euler = (0, 0, a - math.pi / 2)    # local +y points radially out
    return ob


def ports(dark):
    """Cut the openings into the base; their walls take the neon material from the cutter."""
    neon = material("neon", (0, 0, 0), roughness=0.5, emission=NEON_BLUE, strength=NEON_STRENGTH)
    base = bpy.data.objects["base"]
    z = (FOOT_H + BASE_H) / 2
    for name, p in PORTS.items():
        cutter = prism(name, stadium(p["w"], p["h"]), -PORT_DEPTH, 3.0, p["azimuth"], z, neon)
        boolean_difference(base, cutter)
    floor = -PORT_DEPTH + 0.02
    usb, jack, switch = PORTS["usb"], PORTS["jack"], PORTS["switch"]
    # What sits inside, dark against the glow: the Type-C tongue, the jack's bore, the lever (off).
    prism("usb tongue", rect(6.6, 0.7), floor, -0.35, usb["azimuth"], z, dark)
    prism("jack bore", stadium(2.6, 2.6), floor, floor + 0.02, jack["azimuth"], z, dark)
    prism("switch lever", rect(2.6, 2.5, cu=1.5), floor, SWITCH_PROUD, switch["azimuth"], z, dark)


def add_glow(scene):
    """Bloom above the screen's brightness, so only the openings get a halo."""
    tree = bpy.data.node_groups.new("glow", "CompositorNodeTree")
    tree.interface.new_socket("Image", in_out="OUTPUT", socket_type="NodeSocketColor")
    layers = tree.nodes.new("CompositorNodeRLayers")
    glare = tree.nodes.new("CompositorNodeGlare")
    glare.inputs["Type"].default_value = "Bloom"
    glare.inputs["Quality"].default_value = "High"
    glare.inputs["Threshold"].default_value = GLOW_THRESHOLD
    glare.inputs["Size"].default_value = 0.1       # a tight halo; wider reads as a smeared lens
    glare.inputs["Strength"].default_value = 0.6
    out = tree.nodes.new("NodeGroupOutput")
    tree.links.new(glare.inputs["Image"], layers.outputs["Image"])
    tree.links.new(out.inputs["Image"], glare.outputs["Image"])
    scene.compositing_node_group = tree


def unwrap_disc(ob):
    """Map the disc's top face to the square the art was drawn in."""
    r = ACTIVE_D / 2
    mesh = ob.data
    if not mesh.uv_layers:
        mesh.uv_layers.new(name="UVMap")
    uv = mesh.uv_layers.active.data
    for poly in mesh.polygons:
        for li in poly.loop_indices:
            v = mesh.vertices[mesh.loops[li].vertex_index].co
            uv[li].uv = (0.5 + v.x / (2 * r), 0.5 + v.y / (2 * r))


def wood_material():
    """Light brown grained wood from procedural nodes, so no image file or licence is needed.

    A distorted wave texture gives the grain and a colour ramp the two browns. The bump height
    is a steep step at each dark line (the light wood lies deeper) plus a fine pore along the
    grain and a coarser mottle, which also vary the gloss.
    """
    mat = bpy.data.materials.new("wood")
    mat.use_nodes = True
    nt = mat.node_tree
    bsdf = nt.nodes["Principled BSDF"]

    coord = nt.nodes.new("ShaderNodeTexCoord")
    mapping = nt.nodes.new("ShaderNodeMapping")
    # Scale the 600 mm plane down to board size; y is squeezed so the grain runs in long lines.
    mapping.inputs["Scale"].default_value = (0.012, 0.0016, 0.012)

    wave = nt.nodes.new("ShaderNodeTexWave")
    wave.wave_type = "BANDS"
    wave.bands_direction = "X"
    wave.wave_profile = "SIN"
    wave.inputs["Scale"].default_value = 26.0
    wave.inputs["Distortion"].default_value = 9.0
    wave.inputs["Detail"].default_value = 4.0
    wave.inputs["Detail Scale"].default_value = 1.1

    ramp = nt.nodes.new("ShaderNodeValToRGB")
    ramp.color_ramp.elements[0].position = 0.34
    ramp.color_ramp.elements[0].color = (*WOOD_DARK, 1.0)
    ramp.color_ramp.elements[1].position = 0.66
    ramp.color_ramp.elements[1].color = (*WOOD_LIGHT, 1.0)

    # Light is low: 1 below the middle of the ramp, 0 a little above it.
    rings = nt.nodes.new("ShaderNodeMapRange")
    rings.inputs["From Min"].default_value = 0.40
    rings.inputs["From Max"].default_value = 0.52
    rings.inputs["To Min"].default_value = 1.0
    rings.inputs["To Max"].default_value = 0.0
    rings.clamp = True

    # The pore: the same coordinates, much finer, and like the grain squeezed along y.
    pore_map = nt.nodes.new("ShaderNodeMapping")
    pore_map.inputs["Scale"].default_value = (0.9, 0.07, 0.9)
    pore = nt.nodes.new("ShaderNodeTexNoise")
    pore.inputs["Scale"].default_value = 1.0
    pore.inputs["Detail"].default_value = 8.0
    pore.inputs["Roughness"].default_value = 0.7

    height = nt.nodes.new("ShaderNodeMath")
    height.operation = "ADD"
    pore_weight = nt.nodes.new("ShaderNodeMath")
    pore_weight.operation = "MULTIPLY"
    pore_weight.inputs[1].default_value = 0.8

    # A coarser mottle, a few millimetres across, so the surface is uneven between the pores.
    mottle = nt.nodes.new("ShaderNodeTexNoise")
    mottle.inputs["Scale"].default_value = 0.35
    mottle.inputs["Detail"].default_value = 6.0
    mottle.inputs["Roughness"].default_value = 0.8
    mottle_weight = nt.nodes.new("ShaderNodeMath")
    mottle_weight.operation = "MULTIPLY"
    mottle_weight.inputs[1].default_value = 0.6
    rough = nt.nodes.new("ShaderNodeMath")
    rough.operation = "ADD"

    bump = nt.nodes.new("ShaderNodeBump")
    bump.inputs["Strength"].default_value = 1.0
    bump.inputs["Distance"].default_value = 1.2

    gloss = nt.nodes.new("ShaderNodeMapRange")
    gloss.inputs["To Min"].default_value = 0.72
    gloss.inputs["To Max"].default_value = 1.0

    nt.links.new(mapping.inputs["Vector"], coord.outputs["Object"])
    nt.links.new(wave.inputs["Vector"], mapping.outputs["Vector"])
    nt.links.new(ramp.inputs["Fac"], wave.outputs["Fac"])
    nt.links.new(rings.inputs["Value"], wave.outputs["Fac"])
    nt.links.new(pore_map.inputs["Vector"], coord.outputs["Object"])
    nt.links.new(pore.inputs["Vector"], pore_map.outputs["Vector"])
    nt.links.new(pore_weight.inputs[0], pore.outputs["Fac"])
    nt.links.new(height.inputs[0], rings.outputs["Result"])
    nt.links.new(mottle.inputs["Vector"], coord.outputs["Object"])
    nt.links.new(mottle_weight.inputs[0], mottle.outputs["Fac"])
    nt.links.new(rough.inputs[0], pore_weight.outputs["Value"])
    nt.links.new(rough.inputs[1], mottle_weight.outputs["Value"])
    nt.links.new(height.inputs[1], rough.outputs["Value"])
    nt.links.new(bump.inputs["Height"], height.outputs["Value"])
    nt.links.new(gloss.inputs["Value"], pore.outputs["Fac"])
    nt.links.new(bsdf.inputs["Base Color"], ramp.outputs["Color"])
    nt.links.new(bsdf.inputs["Normal"], bump.outputs["Normal"])
    nt.links.new(bsdf.inputs["Roughness"], gloss.outputs["Result"])
    bsdf.inputs["Specular IOR Level"].default_value = 0.35
    return mat


def build_ground(dark, transparent=False, style="wood"):
    """The plane the device stands on: wood or plain grey.

    With `transparent` it becomes a shadow catcher, so the contact shadow survives the cut-out.
    """
    bpy.ops.mesh.primitive_plane_add(size=600, location=(0, 0, 0))
    ob = bpy.context.object
    ob.name = "ground"
    if style == "wood" and not dark and not transparent:
        ob.data.materials.append(wood_material())
    else:
        ob.data.materials.append(
            material("ground", GROUND_DARK if dark else GROUND_LIGHT, roughness=0.75))
    if transparent:
        ob.is_shadow_catcher = True


def build_lights(dark, ground="grey"):
    """Soft area light from the upper left, plus a cool fill from the right."""
    # The key has to stay well above the ambient, or the drop shadow washes out.
    key_energy = 900 if not dark else 380
    if ground == "wood":
        # A sun, not an area light: an area light this close has a penumbra wider than the
        # device is tall, so the drop shadow dissolves; a sun also lights the board evenly.
        # Placed opposite the lamp so the shadow falls towards the camera in both views.
        bpy.ops.object.light_add(type="SUN", location=(-150, 105, 190))
        key = bpy.context.object
        key.data.energy = 4.2
        key.data.angle = math.radians(4.0)   # how soft the shadow's edge is
    else:
        bpy.ops.object.light_add(type="AREA", location=(-140, -90, 190))
        key = bpy.context.object
        key.data.size = 220
        key.data.energy = key_energy
    key.rotation_euler = aim_at(key.location, Vector((0, 0, KNOB_H)))

    bpy.ops.object.light_add(type="AREA", location=(150, 60, 90))
    fill = bpy.context.object
    fill.data.size = 180
    fill.data.energy = key_energy * (0.18 if not dark else 0.30)
    fill.data.color = (0.62, 0.82, 1.0)
    fill.rotation_euler = aim_at(fill.location, Vector((0, 0, KNOB_H)))

    world = bpy.data.worlds.new("world")
    bpy.context.scene.world = world
    world.use_nodes = True
    bg = world.node_tree.nodes["Background"]
    bg.inputs[0].default_value = (0.55, 0.57, 0.60, 1) if not dark else (0.03, 0.03, 0.04, 1)
    bg.inputs[1].default_value = (0.45 if ground == "wood" else 0.6) if not dark else 0.35


def aim_at(loc, target):
    d = Vector(target) - Vector(loc)
    return d.to_track_quat("-Z", "Y").to_euler()


def add_camera(name, loc, target, lens=85):
    bpy.ops.object.camera_add(location=loc)
    cam = bpy.context.object
    cam.name = name
    cam.data.lens = lens
    cam.rotation_euler = aim_at(Vector(loc), Vector(target))
    return cam


# The two views, written as angles rather than coordinates: elevation above the ground,
# azimuth round the device (-90 puts the camera on the -y side, so the USB socket faces the
# lens), distance from the point being looked at.
CAMERAS = {
    # Three degrees off vertical, so the rim and the device's height stay visible.
    "front": dict(elevation=87, azimuth=-90, distance=300, target=(0, 0, KNOB_H), lens=110),
    # Shows wall, flutes and all four openings without flattening the glass into a narrow ellipse.
    "quarter": dict(elevation=40, azimuth=-72, distance=262, target=(0, 0, 14), lens=90),
}


def camera_at(elevation, azimuth, distance, target):
    """Put a camera `distance` away from `target`, `elevation` degrees up, `azimuth` round."""
    e, a = math.radians(elevation), math.radians(azimuth)
    return (Vector(target)
            + Vector((math.cos(e) * math.cos(a), math.cos(e) * math.sin(a), math.sin(e)))
            * distance)


def render(view, dark, out_dir, width, samples, art_dir, transparent=False, ground="wood"):
    clear()
    scene = bpy.context.scene
    scene.render.engine = "CYCLES"
    scene.cycles.device = "CPU"
    scene.cycles.samples = samples
    scene.cycles.use_denoising = True
    scene.render.resolution_x = width
    scene.render.resolution_y = int(width * 1.25) if view != "front" else width
    scene.render.film_transparent = transparent
    scene.view_settings.view_transform = "AgX"
    scene.view_settings.look = "AgX - Punchy"

    # A view whose screen art is missing falls back to home.png.
    art = os.path.join(art_dir, SCREEN_ART[view]) if art_dir else None
    if art and not os.path.exists(art):
        home = os.path.join(art_dir, "home.png")
        art = home if os.path.exists(home) else art
    build_device(art)
    build_ground(dark, transparent, ground)
    build_lights(dark, ground if not transparent else "grey")
    add_glow(scene)
    spec = CAMERAS[view]
    loc = camera_at(spec["elevation"], spec["azimuth"], spec["distance"], spec["target"])
    cam = add_camera("cam", loc, spec["target"], spec["lens"])
    scene.camera = cam

    name = f"{view}-dark.png" if dark else f"{view}.png"
    scene.render.filepath = os.path.join(out_dir, name)
    scene.render.image_settings.file_format = "PNG"
    scene.render.image_settings.color_mode = "RGBA" if transparent else "RGB"
    print(f"[render] {name} {scene.render.resolution_x}x{scene.render.resolution_y} "
          f"{samples} samples", flush=True)
    bpy.ops.render.render(write_still=True)


def main():
    out = os.path.expanduser(flag("--out", "target/render"))
    os.makedirs(out, exist_ok=True)
    quick = flag("--quick", store=True)
    width = int(flag("--width", 800 if quick else 1600))
    samples = int(flag("--samples", 24 if quick else 160))
    views = flag("--views", "front,quarter").split(",")
    transparent = flag("--transparent", store=True)
    ground = flag("--ground", "wood")
    art_dir = os.path.expanduser(flag("--art", os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "..", "media", "screen")))

    for view in views:
        view = view.strip()
        if view not in CAMERAS:
            print(f"[render] unknown view {view!r}, skipping", flush=True)
            continue
        render(view, False, out, width, samples, art_dir, transparent, ground)
        if flag("--dark", store=True):
            render(view, True, out, width, samples, art_dir, transparent, ground)
    print(f"[render] done, images in {out}", flush=True)


main()
