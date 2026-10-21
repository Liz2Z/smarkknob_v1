"""Topology, coordinate, packaging, and offline safety checks for the read-only viewer."""
import json
import math
from pathlib import Path
import unittest

from server import render_panel
from wiring.check_design import validate, transform

ROOT = Path(__file__).resolve().parent


class WiringTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.design = json.loads((ROOT / 'wiring/design.json').read_text())

    def test_both_layouts_have_attached_clear_connected_routes(self):
        for layout in (self.design, self.design['exploded']):
            result = validate(layout)
            self.assertTrue(result['ok'], result['errors'])
            self.assertEqual(result['net_count'], 13)
            self.assertGreaterEqual(result['minimum_surface_gap_mm'], .4)

    def test_all_wires_are_axis_aligned_with_right_angle_corners(self):
        for layout in (self.design, self.design['exploded']):
            pads = {f"{m['id']}:{p['id']}": transform(m, p['position'])
                    for m in layout['modules'] for p in m['pins']}
            for net in layout['nets']:
                for path in net['paths']:
                    points = [pads[p['pin']] if isinstance(p, dict) else p for p in path]
                    axes = []
                    for a, b in zip(points, points[1:]):
                        changed = [i for i in range(3) if abs(b[i] - a[i]) > 1e-8]
                        self.assertEqual(len(changed), 1, (net['id'], a, b))
                        axes.append(changed[0])
                    for before, after in zip(axes, axes[1:]):
                        self.assertNotEqual(before, after, net['id'])

    def test_layout_change_preserves_electrical_identity_and_local_pin_order(self):
        self.assertEqual(
            [(n['id'], n['pins']) for n in self.design['nets']],
            [(n['id'], n['pins']) for n in self.design['exploded']['nets']])
        for normal, expanded in zip(self.design['modules'], self.design['exploded']['modules']):
            self.assertEqual(normal['pins'], expanded['pins'])
            if normal['id'] == 'encoder':
                p = next(p for p in normal['pins'] if p['id'] == 'SCL')
                a, b = transform(normal, p['position']), transform(expanded, p['position'])
                self.assertEqual([b[i] - a[i] for i in range(3)], [0, 25, 0])
            else:
                self.assertEqual(normal['position'], expanded['position'])

    def test_actual_gpio_and_power_nets_remain_fixed(self):
        nets = {n['id']: set(n['pins']) for n in self.design['nets']}
        for name, expected in {
            'in1': {'c3:4', 'driver:IN1'},
            'in2': {'c3:5', 'driver:IN2'},
            'in3': {'c3:6', 'driver:IN3'},
            'en': {'c3:7', 'driver:EN'},
            'fault': {'c3:10', 'driver:nFT'},
            'sda': {'c3:20', 'encoder:SDA'},
            'scl': {'c3:21', 'encoder:SCL'},
            'vm': {'supply:+12V', 'driver:VM'},
            'v3': {'c3:3V3', 'driver:nSP', 'driver:nRT', 'encoder:VCC'},
        }.items():
            self.assertEqual(nets[name], expected)
        self.assertIn('supply:GND', nets['gnd'])
        self.assertIn('driver:GND_POWER', nets['gnd'])
        self.assertFalse(any('driver:3V3_OUT' in refs for refs in nets.values()))

    def test_routes_stay_short_and_endpoints_follow_pads(self):
        for layout in (self.design, self.design['exploded']):
            pads = {f"{m['id']}:{p['id']}": transform(m, p['position'])
                    for m in layout['modules'] for p in m['pins']}
            for net in layout['nets']:
                for path in net['paths']:
                    self.assertIsInstance(path[0], dict)
                    self.assertIsInstance(path[-1], dict)
                    points = [pads[p['pin']] if isinstance(p, dict) else p for p in path]
                    length = sum(math.dist(a, b) for a, b in zip(points, points[1:]))
                    if net['id'] in ('u', 'v', 'w'):
                        self.assertLess(length, 25)
                    if {path[0]['pin'], path[-1]['pin']} == {'driver:nSP', 'driver:nRT'}:
                        self.assertLess(length, 10)
            # No private overhead lanes above the tallest connected module.
            highest_pad = max(p[1] for p in pads.values())
            self.assertTrue(all(p[1] <= highest_pad for n in layout['nets'] for path in n['paths']
                                for p in path if isinstance(p, list)))
        self.assertLess(validate(self.design)['centreline_length_mm'], 900)

    def test_routes_clear_driver_capacitor_envelope(self):
        for layout in (self.design, self.design['exploded']):
            driver = next(m for m in layout['modules'] if m['id'] == 'driver')
            cap = next(c for c in driver['components'] if c['shape'] == 'cylinder')
            center = transform(driver, cap['position'])
            pads = {f"{m['id']}:{p['id']}": transform(m, p['position'])
                    for m in layout['modules'] for p in m['pins']}
            for net in layout['nets']:
                margin = net['diameter']/2 + layout['required_clearance_mm']
                half = [cap['radius']+margin, cap['height']/2+margin, cap['radius']+margin]
                lo, hi = [[center[i]+sign*half[i] for i in range(3)] for sign in (-1, 1)]
                for path in net['paths']:
                    points = [pads[p['pin']] if isinstance(p, dict) else p for p in path]
                    for a, b in zip(points, points[1:]):
                        t0, t1 = 0., 1.
                        for i in range(3):
                            if abs(b[i]-a[i]) < 1e-8:
                                if not lo[i] <= a[i] <= hi[i]:
                                    t0, t1 = 1., 0.
                                    break
                            else:
                                enter, leave = sorted(((lo[i]-a[i])/(b[i]-a[i]),
                                                       (hi[i]-a[i])/(b[i]-a[i])))
                                t0, t1 = max(t0, enter), min(t1, leave)
                        self.assertGreater(t0, t1, (net['id'], a, b))

    def test_panel_bundles_all_viewer_assets_without_external_runtime(self):
        html = render_panel().decode()
        self.assertNotIn('<!--WIRING_', html)
        self.assertEqual(html.count('id="wiringPage"'), 1)
        self.assertEqual(html.count('id="wiring-design"'), 1)
        self.assertIn('href="#wiring"', html)
        text = html.split('id="wiring-design">')[1].split('</script>')[0]
        self.assertEqual(json.loads(text), self.design)
        viewer = (ROOT / 'wiring/viewer.js').read_text()
        self.assertNotIn('fetch(', viewer)
        self.assertNotIn('/api/', viewer)
        self.assertNotIn('<script src=', html)


if __name__ == '__main__':
    unittest.main()
