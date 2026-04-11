; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [6 x i8] c"hello\00", align 1
@.str.1 = private unnamed_addr constant [6 x i8] c"world\00", align 1
@.str.2 = private unnamed_addr constant [4 x i8] c"abc\00", align 1
@.str.true = private unnamed_addr constant [5 x i8] c"true\00", align 1
@.str.false = private unnamed_addr constant [6 x i8] c"false\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)
declare i32 @strcmp(ptr, ptr)

define i32 @main() {
entry:
  %t0 = call i32 @strcmp(ptr @.str.0, ptr @.str.0)
  %t1 = icmp eq i32 %t0, 0
  %t3 = select i1 %t1, ptr @.str.true, ptr @.str.false
  call i32 @puts(ptr %t3)
  %t5 = call i32 @strcmp(ptr @.str.0, ptr @.str.1)
  %t6 = icmp eq i32 %t5, 0
  %t8 = select i1 %t6, ptr @.str.true, ptr @.str.false
  call i32 @puts(ptr %t8)
  %t10 = call i32 @strcmp(ptr @.str.0, ptr @.str.1)
  %t11 = icmp ne i32 %t10, 0
  %t13 = select i1 %t11, ptr @.str.true, ptr @.str.false
  call i32 @puts(ptr %t13)
  %t15 = call i32 @strcmp(ptr @.str.2, ptr @.str.2)
  %t16 = icmp eq i32 %t15, 0
  %t18 = select i1 %t16, ptr @.str.true, ptr @.str.false
  call i32 @puts(ptr %t18)
  ret i32 0
}

