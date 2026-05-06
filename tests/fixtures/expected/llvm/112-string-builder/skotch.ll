; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [6 x i8] c"Hello\00", align 1
@.str.1 = private unnamed_addr constant [3 x i8] c", \00", align 1
@.str.2 = private unnamed_addr constant [6 x i8] c"World\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)

define i32 @main() {
entry:
  %t0 = inttoptr i64 0 to ptr
  %t1 = inttoptr i64 0 to ptr
  %t2 = inttoptr i64 0 to ptr
  %t3 = inttoptr i64 0 to ptr
  %t4 = inttoptr i64 0 to ptr
  call i32 @puts(ptr %t4)
  ret i32 0
}

