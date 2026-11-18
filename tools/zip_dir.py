"""zip_dir.py SRC_DIR OUT.zip -- zip SRC_DIR with its folder name as the top level (forward-slash entries)."""
import os
import sys
import zipfile

src, out = os.path.abspath(sys.argv[1]), sys.argv[2]
base = os.path.basename(src)
n = 0
with zipfile.ZipFile(out + '.tmp', 'w', zipfile.ZIP_DEFLATED, compresslevel=9) as z:
    for root, dirs, files in os.walk(src):
        dirs.sort()
        rel = os.path.relpath(root, src)
        prefix = base + '/' + ('' if rel == '.' else rel.replace(os.sep, '/') + '/')
        for f in sorted(files):
            z.write(os.path.join(root, f), prefix + f)
            n += 1
os.replace(out + '.tmp', out)
print(n, 'files,', os.path.getsize(out) // (1024 * 1024), 'MB')
