; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [7 x i8] c"circle\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)

define i32 @main() {
entry:
  %t0 = inttoptr i64 0 to ptr
  %t1 = inttoptr i64 0 to ptr
  call i32 @puts(ptr %t1)
  ret i32 0
}

