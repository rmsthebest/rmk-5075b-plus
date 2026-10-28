// After auto-analysis: writes the views notes.md is built from.
//   decomp.c     every function, decompiled, in address order
//   functions.txt  address, size, name, callers -> callees
//   mmio.txt     every peripheral/SCS access: function, instruction, R/W, register
//   calls.txt    every reference from the ROM to code or data outside it
//                (flash, SRAM, information blocks): what the ROM depends on
//   strings.txt  defined strings (USB string descriptors are UTF-16)
// Args: <out dir>
//@category WB32

import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileResults;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.data.PointerDataType;
import ghidra.program.model.listing.Data;
import ghidra.program.model.listing.Function;
import ghidra.program.model.listing.Instruction;
import ghidra.program.model.mem.MemoryBlock;
import ghidra.program.model.symbol.Reference;
import ghidra.program.model.symbol.Symbol;

import java.io.File;
import java.io.PrintWriter;
import java.util.TreeSet;

public class RomExport extends GhidraScript {

    @Override
    protected void run() throws Exception {
        String[] args = getScriptArgs();
        File out = new File(args.length > 0 ? args[0] : "out");
        out.mkdirs();
        MemoryBlock rom = currentProgram.getMemory().getBlock("rom");
        int typed = typeLiteralPointers(rom);
        println("RomExport: " + typed + " literal-pool words typed as pointers");

        try (PrintWriter w = new PrintWriter(new File(out, "decomp.c"))) {
            DecompInterface ifc = new DecompInterface();
            ifc.openProgram(currentProgram);
            for (Function f : currentProgram.getFunctionManager().getFunctions(true)) {
                DecompileResults r = ifc.decompileFunction(f, 120, monitor);
                w.println("// " + f.getEntryPoint() + " " + f.getName() + ", " + f.getBody().getNumAddresses()
                        + " bytes");
                w.println(r.decompileCompleted() ? r.getDecompiledFunction().getC()
                        : "// decompile failed: " + r.getErrorMessage() + "\n");
            }
            ifc.dispose();
        }

        try (PrintWriter w = new PrintWriter(new File(out, "functions.txt"))) {
            for (Function f : currentProgram.getFunctionManager().getFunctions(true)) {
                TreeSet<String> callers = new TreeSet<>();
                TreeSet<String> callees = new TreeSet<>();
                for (Function c : f.getCallingFunctions(monitor)) {
                    callers.add(c.getName());
                }
                for (Function c : f.getCalledFunctions(monitor)) {
                    callees.add(c.getName());
                }
                w.printf("%s %5d %-32s %s -> %s%n", f.getEntryPoint(), f.getBody().getNumAddresses(), f.getName(),
                        callers, callees);
            }
        }

        try (PrintWriter mmio = new PrintWriter(new File(out, "mmio.txt"));
                PrintWriter calls = new PrintWriter(new File(out, "calls.txt"))) {
            for (Instruction ins : currentProgram.getListing().getInstructions(rom.getStart(), true)) {
                if (!rom.contains(ins.getAddress())) {
                    break;
                }
                for (Reference ref : ins.getReferencesFrom()) {
                    Address to = ref.getToAddress();
                    MemoryBlock blk = currentProgram.getMemory().getBlock(to);
                    if (blk == null || blk == rom) {
                        continue;
                    }
                    String line = String.format("%s %-28s %-24s %-6s %s %s", ins.getAddress(), fn(ins.getAddress()),
                            ins.toString(), ref.getReferenceType().getName(), to, name(to));
                    if (blk.isVolatile()) {
                        mmio.println(line);
                    } else {
                        calls.println(blk.getName() + " " + line);
                    }
                }
            }
        }

        try (PrintWriter w = new PrintWriter(new File(out, "strings.txt"))) {
            for (Data d : currentProgram.getListing().getDefinedData(true)) {
                if (d.hasStringValue()) {
                    w.println(d.getAddress() + " " + d.getDataType().getName() + " " + d.getDefaultValueRepresentation());
                }
            }
        }
        println("RomExport: wrote " + out);
    }

    /**
     * Literal-pool words that hold an address outside the ROM (a register,
     * SRAM, the information blocks) become pointers, so the decompiler
     * shows `RCC_USBPRE` instead of `*DAT_1fffed1c`.
     */
    private int typeLiteralPointers(MemoryBlock rom) throws Exception {
        int n = 0;
        for (Instruction ins : currentProgram.getListing().getInstructions(rom.getStart(), true)) {
            if (!rom.contains(ins.getAddress())) {
                break;
            }
            for (Reference ref : ins.getReferencesFrom()) {
                Address lit = ref.getToAddress();
                if (!ref.getReferenceType().isRead() || !rom.contains(lit) || lit.getOffset() % 4 != 0
                        || getInstructionContaining(lit) != null) {
                    continue;
                }
                long w = getInt(lit) & 0xFFFFFFFFL;
                MemoryBlock to = currentProgram.getMemory().getBlock(toAddr(w));
                if (to == null || to == rom) {
                    continue;
                }
                Data d = getDataAt(lit);
                if (d != null && d.isPointer()) {
                    continue;
                }
                clearListing(lit, lit.add(3));
                createData(lit, PointerDataType.dataType);
                n++;
            }
        }
        return n;
    }

    private String fn(Address a) {
        Function f = getFunctionContaining(a);
        return f == null ? "?" : f.getName();
    }

    private String name(Address a) {
        Symbol s = getSymbolAt(a);
        return s == null ? "" : s.getName();
    }
}
