# Counters for m7_power.robot, imported by the hooks it installs in Renode's
# IronPython (each hook has its own scope; this module is the one they share).
#   hit(name)  an interrupt handler was entered (cpu AddHook on its address)
#   sleep()    the CPU went into WFI/WFE (cpu AddHookAtWfiStateChange)
#   clear()    start a measurement
#   report()   "sleeps=<n> <name>=<n> ..." for the robot keyword to parse

counts = {}


def hit(name):
    counts[name] = counts.get(name, 0) + 1


def sleep():
    hit("sleeps")


def clear():
    counts.clear()


def report():
    return " ".join("%s=%d" % (k, counts[k]) for k in sorted(counts))
