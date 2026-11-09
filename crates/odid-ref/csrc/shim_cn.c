// sizeof/offsetof of the GB 46750 struct the Rust side mirrors.
#include <stddef.h>
#include "opendroneidcn.h"

size_t odid_ref_cn_sizeof(int which)
{
    switch (which) {
    case 0: return sizeof(DroneRIDData_t);
    case 1: return offsetof(DroneRIDData_t, gcs_lon);
    case 2: return offsetof(DroneRIDData_t, has_rel_alt);
    case 3: return offsetof(DroneRIDData_t, timestamp_ms);
    case 4: return offsetof(DroneRIDData_t, timestamp_accuracy);
    default: return 0;
    }
}
