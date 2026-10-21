#!/usr/bin/env python3
"""Validate model topology and 3D line-segment clearance; stdlib only."""
import json
import math
from pathlib import Path
import sys


def vector(value, label):
    if not isinstance(value, list) or len(value) != 3 or not all(
        type(v) in (int, float) and math.isfinite(v) for v in value
    ):
        raise ValueError(f'{label}: expected three finite numbers')
    return value


def positive(value, label, zero=False):
    if type(value) not in (int, float) or not math.isfinite(value) or (value < 0 if zero else value <= 0):
        raise ValueError(f'{label}: expected {"nonnegative" if zero else "positive"} finite number')
    return value


def dot(a, b):
    return sum(x*y for x, y in zip(a, b))


def sub(a, b):
    return [x-y for x, y in zip(a, b)]


def segment_distance(p, q, r, s):
    """Closest distance between arbitrary finite 3D segments, including points."""
    u, v, w = sub(q, p), sub(s, r), sub(p, r)
    a, b, c, d, e = dot(u, u), dot(u, v), dot(v, v), dot(u, w), dot(v, w)
    clamp = lambda x: max(0.0, min(1.0, x))
    if a < 1e-16 and c < 1e-16:
        return math.dist(p, r)
    if a < 1e-16:
        x, y = 0.0, clamp(e/c)
    elif c < 1e-16:
        x, y = clamp(-d/a), 0.0
    else:
        determinant = a*c-b*b
        x = clamp((b*e-c*d)/determinant) if determinant > 1e-14*a*c else 0.0
        y = (b*x+e)/c
        if y < 0:
            x, y = clamp(-d/a), 0.0
        elif y > 1:
            x, y = clamp((b-d)/a), 1.0
    return math.sqrt(sum((w[i]+x*u[i]-y*v[i])**2 for i in range(3)))


def transform(module, point, translate=True):
    basis = module.get('basis', [[1, 0, 0], [0, 1, 0], [0, 0, 1]])
    origin = module['position'] if translate else [0, 0, 0]
    return [origin[i]+dot(basis[i], point) for i in range(3)]


def validate(design):
    errors, warnings, pads, segments = [], [], {}, []
    tolerance = positive(design.get('join_tolerance_mm', .02), 'join_tolerance_mm')
    gap = positive(design.get('required_clearance_mm', .4), 'required_clearance_mm', zero=True)
    ids = set()
    for module in design['modules']:
        mid = module['id']
        if not isinstance(mid, str) or not mid or ':' in mid or mid in ids:
            raise ValueError('module IDs must be unique nonempty strings without colons')
        ids.add(mid)
        vector(module['position'], f'{mid}.position')
        basis = module.get('basis', [[1, 0, 0], [0, 1, 0], [0, 0, 1]])
        if not isinstance(basis, list) or len(basis) != 3:
            raise ValueError(f'{mid}: invalid basis')
        for i, row in enumerate(basis):
            vector(row, f'{mid}.basis[{i}]')
        for i in range(3):
            for j in range(3):
                if abs(dot(basis[i], basis[j])-(1 if i == j else 0)) > 1e-6:
                    raise ValueError(f'{mid}: basis must be orthonormal')
        a, b, c = basis
        determinant = a[0]*(b[1]*c[2]-b[2]*c[1])-a[1]*(b[0]*c[2]-b[2]*c[0])+a[2]*(b[0]*c[1]-b[1]*c[0])
        if abs(determinant-1) > 1e-6:
            raise ValueError(f'{mid}: reflection is not a physical rotation')
        board = module['board']
        if board['shape'] == 'rect':
            for k in ['width', 'depth']:
                positive(board[k], f'{mid}.board.{k}')
        elif board['shape'] == 'circle':
            positive(board['diameter'], f'{mid}.board.diameter')
        else:
            raise ValueError(f'{mid}: unsupported board shape')
        positive(board.get('thickness', 1.2), f'{mid}.board.thickness')
        if not module.get('evidence'):
            warnings.append(f'{mid}: dimension and pin evidence not recorded')
        for pin in module['pins']:
            pid = pin['id']
            if not isinstance(pid, str) or not pid or ':' in pid:
                raise ValueError(f'{mid}: invalid pin ID')
            ref = f'{mid}:{pid}'
            if ref in pads:
                raise ValueError(f'duplicate pin {ref}')
            normal = vector(pin.get('normal', [0, 1, 0]), f'{ref}.normal')
            if abs(dot(normal, normal)-1) > 1e-6:
                raise ValueError(f'{ref}: pad normal must be a unit vector')
            pads[ref] = transform(module, vector(pin['position'], f'{ref}.position'))
        for component in module.get('components', []):
            vector(component['position'], f'{mid}.component.position')
            if component['shape'] == 'box':
                for v in vector(component['size'], f'{mid}.component.size'):
                    positive(v, f'{mid}.component.size')
            elif component['shape'] == 'cylinder':
                positive(component['radius'], f'{mid}.component.radius')
                positive(component['height'], f'{mid}.component.height')
            else:
                raise ValueError(f'{mid}: unsupported component shape')
    assigned, net_ids = {}, set()
    total_length = 0
    for net in design['nets']:
        nid = net['id']
        if not isinstance(nid, str) or not nid or nid in net_ids:
            raise ValueError('net IDs must be unique nonempty strings')
        net_ids.add(nid)
        radius = positive(net.get('diameter', .7), f'{nid}.diameter')/2
        refs = net['pins']
        if len(refs) < 2 or len(refs) != len(set(refs)):
            raise ValueError(f'{nid}: expected at least two distinct pads')
        for ref in refs:
            if ref not in pads:
                raise ValueError(f'{nid}: unknown pad {ref}')
            if ref in assigned:
                raise ValueError(f'{ref}: belongs to both {assigned[ref]} and {nid}')
            assigned[ref] = nid
        local = []
        for path in net['paths']:
            if len(path) < 2:
                raise ValueError(f'{nid}: paths need at least two points')
            points = []
            for point in path:
                if isinstance(point, dict):
                    ref = point['pin']
                    if ref not in refs:
                        raise ValueError(f'{nid}: path references a pad outside its net: {ref}')
                    point = pads[ref]
                points.append(vector(point, f'{nid}.path point'))
            for p, q in zip(points, points[1:]):
                length = math.dist(p, q)
                if length < 1e-9:
                    continue
                local.append((p, q, nid, radius))
                total_length += length
        if not local:
            errors.append(f'{nid}: no nonzero wire segments')
            continue
        reached, todo = {0}, [0]
        while todo:
            i = todo.pop()
            for j, candidate in enumerate(local):
                if j not in reached and segment_distance(*local[i][:2], *candidate[:2]) <= tolerance:
                    reached.add(j)
                    todo.append(j)
        if len(reached) != len(local):
            errors.append(f'{nid}: disconnected wire branches')
        for ref in refs:
            if not any(segment_distance(pads[ref], pads[ref], *s[:2]) <= tolerance for s in local):
                errors.append(f'{nid}: wire does not reach {ref}')
        segments.extend(local)
    minimum = math.inf
    for i, first in enumerate(segments):
        for second in segments[i+1:]:
            if first[2] == second[2]:
                continue
            clearance = segment_distance(*first[:2], *second[:2])-first[3]-second[3]
            minimum = min(minimum, clearance)
            if clearance < gap-1e-8:
                errors.append(f'{first[2]} / {second[2]}: surface gap {clearance:.3f} mm < {gap:.3f} mm')
    return {'ok': not errors, 'errors': list(dict.fromkeys(errors)), 'warnings': warnings,
            'module_count': len(ids), 'net_count': len(net_ids), 'segment_count': len(segments),
            'centreline_length_mm': round(total_length, 3),
            'minimum_surface_gap_mm': round(minimum, 3) if math.isfinite(minimum) else None,
            'limits': 'Centreline model only; component collisions, electrical suitability and fabrication tolerances not checked.'}


def self_test():
    import copy
    import unittest
    class Checks(unittest.TestCase):
        def test_segment_geometry(self):
            self.assertAlmostEqual(segment_distance([0,0,0],[2,2,0],[0,2,0],[2,0,0]), 0)
            self.assertAlmostEqual(segment_distance([0,0,0],[2,2,0],[0,2,3],[2,0,3]), 3)
            self.assertAlmostEqual(segment_distance([0,0,0],[1,0,0],[2,0,0],[3,0,0]), 1)
            self.assertAlmostEqual(segment_distance([0,0,0],[0,0,0],[1,-1,0],[1,1,0]), 1)
            # Bounding boxes overlap but the finite diagonal segments do not.
            self.assertAlmostEqual(segment_distance([0,0,0],[2,2,0],[0,1,0],[.3,2,0]), math.sqrt(.5))
        def test_model_failures(self):
            module={'id':'a','position':[0,5,0],'board':{'shape':'rect','width':10,'depth':10},'pins':[{'id':'p','position':[0,0,0]}]}
            other=copy.deepcopy(module);other.update(id='b',position=[0,0,0])
            design={'modules':[module,other],'nets':[{'id':'n','pins':['a:p','b:p'],'paths':[[{'pin':'a:p'},{'pin':'b:p'}]]}]}
            self.assertTrue(validate(design)['ok'])
            broken=copy.deepcopy(design);broken['nets'][0]['paths']=[[[0,4,0],[0,1,0]]]
            self.assertFalse(validate(broken)['ok'])
            mirrored=copy.deepcopy(design);mirrored['modules'][0]['basis']=[[-1,0,0],[0,1,0],[0,0,1]]
            with self.assertRaisesRegex(ValueError,'reflection'):validate(mirrored)
            short=copy.deepcopy(design)
            for m in short['modules']:m['pins'].append({'id':'q','position':[.5,0,0]})
            short['nets'].append({'id':'short','pins':['a:q','b:q'],'paths':[[{'pin':'a:q'},{'pin':'b:q'}]]})
            self.assertFalse(validate(short)['ok'])
    result=unittest.TextTestRunner().run(unittest.defaultTestLoader.loadTestsFromTestCase(Checks))
    return 0 if result.wasSuccessful() else 1


if __name__ == '__main__':
    if sys.argv[1:] == ['--self-test']:
        sys.exit(self_test())
    if len(sys.argv) != 2:
        sys.exit('Usage: check_design.py design.json | --self-test')
    try:
        report = validate(json.loads(Path(sys.argv[1]).read_text()))
    except (ValueError, KeyError, TypeError) as error:
        report = {'ok': False, 'errors': [str(error)]}
    print(json.dumps(report, ensure_ascii=False, indent=2))
    sys.exit(0 if report['ok'] else 1)
