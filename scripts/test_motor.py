import unittest

from motor import establish_console, maintain_continuous


class ConsoleStartupTests(unittest.TestCase):
    def test_command_discarded_during_boot_is_followed_by_status_only(self):
        commands = []

        def send(command, expected, timeout):
            commands.append(command)
            if len(commands) == 1:
                raise RuntimeError("Only ROM boot output arrived; command was lost")

        establish_console(send)
        self.assertEqual(commands, [b"?", b"?"])

    def test_unresponsive_device_fails_after_bounded_status_queries(self):
        commands = []

        def send(command, expected, timeout):
            commands.append(command)
            raise RuntimeError("Device never became ready")

        with self.assertRaisesRegex(RuntimeError, "never became ready"):
            establish_console(send)
        self.assertEqual(commands, [b"?", b"?", b"?"])

    def test_continuous_keepalive_does_not_restart_after_fault(self):
        commands = []

        def send(command, expected, timeout):
            commands.append(command)
            raise RuntimeError("FAULT LOW")

        with self.assertRaisesRegex(RuntimeError, "FAULT LOW"):
            maintain_continuous(send, 0)
        self.assertEqual(commands, [b"k"])

    def test_bounded_trial_sends_only_heartbeats_until_deadline(self):
        clock = [0.0]
        commands = []

        def send(command, expected, timeout):
            commands.append(command)

        def sleep(seconds):
            clock[0] += seconds

        maintain_continuous(send, 0.25, lambda: clock[0], sleep)
        self.assertEqual(commands, [b"k", b"k", b"k"])


if __name__ == "__main__":
    unittest.main()
