// Before auto-analysis: the WB32FQ95 memory map around the imported ROM, the
// vector table (entry points named from out/vectors.csv), and the MMIO and
// memory labels from out/labels.csv.
// Args: <labels.csv> <vectors.csv> [info.bin]
//@category WB32

import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.mem.Memory;
import ghidra.program.model.mem.MemoryBlock;
import ghidra.program.model.symbol.SourceType;

import java.io.File;
import java.io.FileInputStream;
import java.nio.file.Files;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

public class RomSetup extends GhidraScript {

    @Override
    protected void run() throws Exception {
        String[] args = getScriptArgs();
        if (args.length < 2) {
            throw new IllegalArgumentException("args: <labels.csv> <vectors.csv> [info.bin]");
        }
        Memory mem = currentProgram.getMemory();
        MemoryBlock rom = mem.getBlocks()[0];
        rom.setName("rom");
        rom.setWrite(false);
        rom.setExecute(true);

        // Everything the ROM might touch, uninitialised (we don't know the
        // contents), except the information blocks when info.bin is given.
        // Peripherals are volatile so the decompiler keeps every access.
        if (args.length > 2 && new File(args[2]).exists()) {
            addInitialised("info", 0x1FFFF000L, new File(args[2]));
        } else {
            addGaps("info", 0x1FFFF000L, 0x800, false);
        }
        addGaps("flash", 0x08000000L, 0x40000, false);
        addGaps("sram", 0x20000000L, 0x9000, false);
        addGaps("periph", 0x40000000L, 0x18000, true);
        addGaps("dbgmcu", 0xE0042000L, 0x100, true);
        addGaps("scs", 0xE000E000L, 0x1000, true);

        // Vector table: entry 0 is the initial SP; then Thumb addresses
        // inside the ROM, or 0. It ends at the first entry that is neither.
        Map<Integer, String> vectors = readCsv(args[1]);
        long base = rom.getStart().getOffset();
        long end = rom.getEnd().getOffset();
        createDWord(rom.getStart());
        setEOLComment(rom.getStart(), "initial SP");
        for (int i = 1; i < 256; i++) {
            Address slot = rom.getStart().add(4L * i);
            if (slot.getOffset() + 3 > end) {
                break;
            }
            long w = getInt(slot) & 0xFFFFFFFFL;
            boolean reserved = i == 7 || i == 8 || i == 9 || i == 10 || i == 13;
            if (reserved && !(w == 0 || ((w & 1) == 1 && w >= base && w <= end))) {
                // The ROM keeps data in some reserved slots (slot 7 is
                // 0xFFFF0004): not a handler, but not the end of the table.
                createDWord(slot);
                setEOLComment(slot, "vector " + i + " (reserved), data");
                continue;
            }
            if (w != 0 && ((w & 1) == 0 || w < base || w > end)) {
                break;
            }
            createDWord(slot);
            String name = vectors.getOrDefault(i, "irq" + (i - 16));
            setEOLComment(slot, "vector " + i + " " + name);
            if (w == 0) {
                continue;
            }
            Address target = toAddr(w & ~1L);
            String fname = "vec_" + name;
            if (getFunctionAt(target) == null) {
                createFunction(target, fname);
            } else {
                // Shared handler (e.g. every fault to one loop): add a label.
                createLabel(target, fname, false, SourceType.USER_DEFINED);
            }
            addEntryPoint(target);
        }

        Map<Integer, String> labels = readCsvLong(args[0]);
        for (Map.Entry<Integer, String> e : labels.entrySet()) {
            Address a = toAddr(e.getKey() & 0xFFFFFFFFL);
            if (mem.contains(a)) {
                createLabel(a, e.getValue(), true, SourceType.USER_DEFINED);
            }
        }
        println("RomSetup: rom " + rom.getStart() + "-" + rom.getEnd() + ", " + labels.size() + " labels");
    }

    /** Uninitialised blocks over the parts of [start, start+size) not already mapped. */
    private void addGaps(String name, long start, long size, boolean isVolatile) throws Exception {
        Memory mem = currentProgram.getMemory();
        long a = start;
        long stop = start + size;
        int n = 0;
        while (a < stop) {
            MemoryBlock b = mem.getBlock(toAddr(a));
            if (b != null) {
                a = b.getEnd().getOffset() + 1;
                continue;
            }
            long next = stop;
            for (MemoryBlock o : mem.getBlocks()) {
                long s = o.getStart().getOffset();
                if (s > a && s < next) {
                    next = s;
                }
            }
            MemoryBlock blk = mem.createUninitializedBlock(n == 0 ? name : name + n, toAddr(a), next - a, false);
            blk.setRead(true);
            blk.setWrite(true);
            blk.setVolatile(isVolatile);
            n++;
            a = next;
        }
    }

    private void addInitialised(String name, long start, File f) throws Exception {
        try (FileInputStream in = new FileInputStream(f)) {
            MemoryBlock blk = currentProgram.getMemory().createInitializedBlock(name, toAddr(start), in,
                    f.length(), monitor, false);
            blk.setWrite(false);
        }
    }

    private Map<Integer, String> readCsv(String path) throws Exception {
        Map<Integer, String> m = new HashMap<>();
        for (String line : Files.readAllLines(new File(path).toPath())) {
            String[] p = line.split(",");
            if (p.length == 2) {
                m.put(Integer.decode(p[0]), p[1]);
            }
        }
        return m;
    }

    private Map<Integer, String> readCsvLong(String path) throws Exception {
        Map<Integer, String> m = new HashMap<>();
        List<String> lines = Files.readAllLines(new File(path).toPath());
        for (String line : lines) {
            String[] p = line.split(",");
            if (p.length == 2) {
                m.put((int) Long.decode(p[0]).longValue(), p[1]);
            }
        }
        return m;
    }
}
