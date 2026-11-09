// Reports sizeof() of the core-c structs the Rust side mirrors, so a layout
// mismatch fails a test instead of silently corrupting memory.
#include <stddef.h>
#include "opendroneid.h"

size_t odid_ref_sizeof(int which)
{
    switch (which) {
    case 0: return sizeof(ODID_BasicID_data);
    case 1: return sizeof(ODID_Location_data);
    case 2: return sizeof(ODID_Auth_data);
    case 3: return sizeof(ODID_SelfID_data);
    case 4: return sizeof(ODID_System_data);
    case 5: return sizeof(ODID_OperatorID_data);
    case 6: return sizeof(ODID_UAS_Data);
    case 7: return sizeof(ODID_MessagePack_data);
    case 8: return sizeof(ODID_MessagePack_encoded);
    case 9: return offsetof(ODID_UAS_Data, Location);
    case 10: return offsetof(ODID_UAS_Data, Auth);
    case 11: return offsetof(ODID_UAS_Data, SelfID);
    case 12: return offsetof(ODID_UAS_Data, System);
    case 13: return offsetof(ODID_UAS_Data, OperatorID);
    case 14: return offsetof(ODID_UAS_Data, BasicIDValid);
    case 15: return offsetof(ODID_UAS_Data, OperatorIDValid);
    case 16: return offsetof(ODID_Location_data, TimeStamp);
    case 17: return offsetof(ODID_System_data, Timestamp);
    case 18: return offsetof(ODID_Auth_data, AuthData);
    default: return 0;
    }
}
