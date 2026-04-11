; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [6 x i8] c"World\00", align 1
@.str.1 = private unnamed_addr constant [8 x i8] c"Hello, \00", align 1
@.str.2 = private unnamed_addr constant [2 x i8] c"!\00", align 1
@.str.3 = private unnamed_addr constant [8 x i8] c"Count: \00", align 1
@.str.4 = private unnamed_addr constant [9 x i8] c"Result: \00", align 1
@.str.5 = private unnamed_addr constant [7 x i8] c" items\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)
@.fmt.concat.ss = private unnamed_addr constant [5 x i8] c"%s%s\00", align 1
@.fmt.concat.sd = private unnamed_addr constant [5 x i8] c"%s%d\00", align 1
declare i32 @snprintf(ptr, i32, ptr, ...)

define i32 @main() {
entry:
  %t0 = add i32 0, 42
  %t1 = alloca [256 x i8]
  call i32 (ptr, i32, ptr, ...) @snprintf(ptr %t1, i32 256, ptr @.fmt.concat.ss, ptr @.str.1, ptr @.str.0)
  %t3 = alloca [256 x i8]
  call i32 (ptr, i32, ptr, ...) @snprintf(ptr %t3, i32 256, ptr @.fmt.concat.ss, ptr %t1, ptr @.str.2)
  call i32 @puts(ptr %t3)
  %t6 = alloca [256 x i8]
  call i32 (ptr, i32, ptr, ...) @snprintf(ptr %t6, i32 256, ptr @.fmt.concat.sd, ptr @.str.3, i32 %t0)
  call i32 @puts(ptr %t6)
  %t9 = alloca [256 x i8]
  call i32 (ptr, i32, ptr, ...) @snprintf(ptr %t9, i32 256, ptr @.fmt.concat.sd, ptr @.str.4, i32 %t0)
  %t11 = alloca [256 x i8]
  call i32 (ptr, i32, ptr, ...) @snprintf(ptr %t11, i32 256, ptr @.fmt.concat.ss, ptr %t9, ptr @.str.5)
  call i32 @puts(ptr %t11)
  ret i32 0
}

